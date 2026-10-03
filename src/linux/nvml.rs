//! NVIDIA GPU temperatures through NVML, loaded at run time with `dlopen`, so
//! `tuxctl` neither links against nor requires the proprietary driver. All of
//! the crate's NVIDIA-related unsafe code is in this file.
//!
//! The types hold raw pointers and are therefore neither `Send` nor `Sync`:
//! they are created, used and dropped on the metrics worker thread, and
//! nothing here touches terminal state.

use std::{
    ffi::{c_char, c_int, c_uint, c_void, CStr, CString},
    ptr,
    time::{Duration, Instant},
};

use super::temperature::{NvidiaReading, NvidiaSource};

const LIBRARY: &CStr = c"libnvidia-ml.so.1";
/// A missing library or a failed initialization is retried this often.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

const NVML_SUCCESS: c_int = 0;
const NVML_ERROR_INVALID_ARGUMENT: c_int = 2;
const NVML_ERROR_NOT_FOUND: c_int = 6;
/// `NVML_TEMPERATURE_GPU` of `nvmlTemperatureSensors_t`: the GPU die.
const NVML_TEMPERATURE_GPU: c_int = 0;
/// `NVML_TEMPERATURE_THRESHOLD_SLOWDOWN` of `nvmlTemperatureThresholds_t`.
const NVML_TEMPERATURE_THRESHOLD_SLOWDOWN: c_int = 1;

/// `nvmlDevice_t`, an opaque pointer.
type Device = *mut c_void;
type InitFn = unsafe extern "C" fn() -> c_int;
type ShutdownFn = unsafe extern "C" fn() -> c_int;
type HandleByBusIdFn = unsafe extern "C" fn(*const c_char, *mut Device) -> c_int;
/// `nvmlDeviceGetTemperature` and `nvmlDeviceGetTemperatureThreshold`.
type TemperatureFn = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> c_int;
type UtilizationFn = unsafe extern "C" fn(Device, *mut Utilization) -> c_int;
type MemoryFn = unsafe extern "C" fn(Device, *mut Memory) -> c_int;
/// `nvmlDeviceGetPowerUsage` (milliwatts) and `nvmlDeviceGetFanSpeed` (%).
type UintFn = unsafe extern "C" fn(Device, *mut c_uint) -> c_int;

/// `nvmlUtilization_t`.
#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

/// `nvmlMemory_t` (version 1).
#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

/// The loaded library and the entry points `tuxctl` uses. The first four are
/// required; the others are read when the driver has them.
struct Library {
    handle: *mut c_void,
    init: InitFn,
    shutdown: ShutdownFn,
    handle_by_bus_id: HandleByBusIdFn,
    temperature: TemperatureFn,
    threshold: Option<TemperatureFn>,
    utilization: Option<UtilizationFn>,
    memory: Option<MemoryFn>,
    power: Option<UintFn>,
    fan: Option<UintFn>,
}

impl Library {
    fn load() -> Option<Self> {
        // SAFETY: `LIBRARY` is a NUL-terminated string. Loading runs the
        // library's initializers, which is what loading it is for.
        let handle = unsafe { libc::dlopen(LIBRARY.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return None;
        }
        let symbol = |name: &CStr| {
            // SAFETY: `handle` is a live handle from `dlopen` and `name` is
            // NUL-terminated.
            let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
            (!address.is_null()).then_some(address)
        };
        let symbols = (|| {
            Some((
                symbol(c"nvmlInit_v2")?,
                symbol(c"nvmlShutdown")?,
                symbol(c"nvmlDeviceGetHandleByPciBusId_v2")?,
                symbol(c"nvmlDeviceGetTemperature")?,
            ))
        })();
        let Some((init, shutdown, handle_by_bus_id, temperature)) = symbols else {
            // SAFETY: `handle` came from a successful `dlopen` and nothing
            // obtained from it is kept.
            unsafe { libc::dlclose(handle) };
            return None;
        };
        // SAFETY: nvml.h declares these symbols with exactly these C
        // signatures: `nvmlReturn_t nvmlInit_v2(void)`, `nvmlReturn_t
        // nvmlShutdown(void)`, `nvmlReturn_t nvmlDeviceGetHandleByPciBusId_v2(
        // const char *, nvmlDevice_t *)`, `nvmlReturn_t
        // nvmlDeviceGetTemperature(nvmlDevice_t, nvmlTemperatureSensors_t,
        // unsigned int *)`, `nvmlDeviceGetTemperatureThreshold(nvmlDevice_t,
        // nvmlTemperatureThresholds_t, unsigned int *)`,
        // `nvmlDeviceGetUtilizationRates(nvmlDevice_t, nvmlUtilization_t *)`,
        // `nvmlDeviceGetMemoryInfo(nvmlDevice_t, nvmlMemory_t *)`,
        // `nvmlDeviceGetPowerUsage(nvmlDevice_t, unsigned int *)` and
        // `nvmlDeviceGetFanSpeed(nvmlDevice_t, unsigned int *)`, where
        // `nvmlReturn_t` and the enums are C enums (int) and the structs match
        // `Utilization` and `Memory`. The pointers stay valid until `dlclose`
        // in `Drop`.
        unsafe {
            Some(Self {
                handle,
                init: std::mem::transmute::<*mut c_void, InitFn>(init),
                shutdown: std::mem::transmute::<*mut c_void, ShutdownFn>(shutdown),
                handle_by_bus_id: std::mem::transmute::<*mut c_void, HandleByBusIdFn>(
                    handle_by_bus_id,
                ),
                temperature: std::mem::transmute::<*mut c_void, TemperatureFn>(temperature),
                threshold: symbol(c"nvmlDeviceGetTemperatureThreshold")
                    .map(|address| std::mem::transmute::<*mut c_void, TemperatureFn>(address)),
                utilization: symbol(c"nvmlDeviceGetUtilizationRates")
                    .map(|address| std::mem::transmute::<*mut c_void, UtilizationFn>(address)),
                memory: symbol(c"nvmlDeviceGetMemoryInfo")
                    .map(|address| std::mem::transmute::<*mut c_void, MemoryFn>(address)),
                power: symbol(c"nvmlDeviceGetPowerUsage")
                    .map(|address| std::mem::transmute::<*mut c_void, UintFn>(address)),
                fan: symbol(c"nvmlDeviceGetFanSpeed")
                    .map(|address| std::mem::transmute::<*mut c_void, UintFn>(address)),
            })
        }
    }

    fn init(&self) -> bool {
        // SAFETY: `nvmlInit_v2` takes no arguments and may be called from any
        // thread; every successful call is balanced by `shutdown`.
        unsafe { (self.init)() == NVML_SUCCESS }
    }

    /// Balances one successful [`Self::init`].
    fn shutdown(&self) {
        // SAFETY: only called after a successful `nvmlInit_v2` that has not
        // been balanced yet (see `NvmlReader::release`).
        unsafe {
            (self.shutdown)();
        }
    }

    /// The device with this PCI bus id. Only valid between [`Self::init`]
    /// and [`Self::shutdown`], like the handle it returns.
    fn device(&self, bus_id: &str) -> Result<Device, c_int> {
        let bus_id = CString::new(bus_id).map_err(|_| NVML_ERROR_INVALID_ARGUMENT)?;
        let mut device: Device = ptr::null_mut();
        // SAFETY: NVML is initialized (caller contract), `bus_id` is
        // NUL-terminated and `device` is a writable handle slot.
        let status = unsafe { (self.handle_by_bus_id)(bus_id.as_ptr(), &mut device) };
        if status == NVML_SUCCESS {
            Ok(device)
        } else {
            Err(status)
        }
    }

    /// What is read for `device`, a handle from [`Self::device`] in the same
    /// initialization: utilization and power, and with `full` also the
    /// temperatures, memory and fan (the fan query alone costs about half a
    /// millisecond on some GPUs).
    fn reading(&self, device: Device, full: bool) -> NvidiaReading {
        let sensor = |function: TemperatureFn, kind: c_int| {
            let mut value: c_uint = 0;
            // SAFETY: `device` is a live handle (caller contract) and `value`
            // is a writable `unsigned int`.
            let status = unsafe { function(device, kind, &mut value) };
            (status == NVML_SUCCESS).then_some(value)
        };
        let count = |function: UintFn| {
            let mut value: c_uint = 0;
            // SAFETY: as above.
            let status = unsafe { function(device, &mut value) };
            (status == NVML_SUCCESS).then_some(value)
        };
        let utilization = self.utilization.and_then(|function| {
            let mut value = Utilization::default();
            // SAFETY: `device` is a live handle and `value` is a writable
            // `nvmlUtilization_t`.
            let status = unsafe { function(device, &mut value) };
            (status == NVML_SUCCESS).then_some(value.gpu)
        });
        let power_milliwatts = self.power.and_then(count);
        if !full {
            return NvidiaReading {
                utilization,
                power_milliwatts,
                ..NvidiaReading::default()
            };
        }
        let memory = self.memory.and_then(|function| {
            let mut value = Memory::default();
            // SAFETY: `device` is a live handle and `value` is a writable
            // `nvmlMemory_t`.
            let status = unsafe { function(device, &mut value) };
            (status == NVML_SUCCESS).then_some((value.used, value.total))
        });
        NvidiaReading {
            temperature: sensor(self.temperature, NVML_TEMPERATURE_GPU).map(i64::from),
            slowdown: self
                .threshold
                .and_then(|function| sensor(function, NVML_TEMPERATURE_THRESHOLD_SLOWDOWN))
                .map(i64::from),
            utilization,
            memory,
            power_milliwatts,
            fan_percent: self.fan.and_then(count),
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful `dlopen`; `NvmlReader` shuts
        // NVML down before dropping the library, and no function pointer
        // outlives `self`.
        unsafe {
            libc::dlclose(self.handle);
        }
    }
}

/// Spaces out attempts after a failure.
#[derive(Debug, Default)]
struct RetryGate {
    next_attempt: Option<Instant>,
}

impl RetryGate {
    fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|next| now >= next)
    }

    fn failed(&mut self, now: Instant) {
        self.next_attempt = Some(now + RETRY_INTERVAL);
    }

    fn succeeded(&mut self) {
        self.next_attempt = None;
    }
}

/// The real [`NvidiaSource`]. Nothing is loaded until the first read, which
/// the sampler only makes when a GPU uses the `nvidia` driver.
#[derive(Default)]
pub(super) struct NvmlReader {
    library: Option<Library>,
    initialized: bool,
    retry: RetryGate,
    /// Bus ids NVML accepted only with an 8-digit PCI domain; at most one
    /// per NVIDIA GPU.
    padded: Vec<String>,
}

impl NvmlReader {
    fn initialize(&mut self, now: Instant) -> bool {
        if self.initialized {
            return true;
        }
        if !self.retry.ready(now) {
            return false;
        }
        if self.library.is_none() {
            self.library = Library::load();
        }
        self.initialized = self.library.as_ref().is_some_and(Library::init);
        if self.initialized {
            self.retry.succeeded();
        } else {
            self.retry.failed(now);
        }
        self.initialized
    }

    /// The GPU at `bus_id`, trying the 8-digit PCI domain when NVML rejects
    /// the 4-digit one and remembering which form worked.
    fn device(&mut self, bus_id: &str) -> Option<Device> {
        let library = self.library.as_ref()?;
        let padded = padded_bus_id(bus_id);
        if let Some(padded) = padded
            .as_deref()
            .filter(|padded| self.padded.iter().any(|known| known == padded))
        {
            return library.device(padded).ok();
        }
        match library.device(bus_id) {
            Ok(device) => Some(device),
            Err(NVML_ERROR_INVALID_ARGUMENT | NVML_ERROR_NOT_FOUND) => {
                let padded = padded?;
                let device = library.device(&padded).ok()?;
                self.padded.push(padded);
                Some(device)
            }
            Err(_) => None,
        }
    }

    fn reading(&mut self, bus_id: &str, full: bool) -> NvidiaReading {
        match (self.device(bus_id), &self.library) {
            (Some(device), Some(library)) => library.reading(device, full),
            _ => NvidiaReading::default(),
        }
    }
}

impl NvidiaSource for NvmlReader {
    fn read(
        &mut self,
        bus_ids: &[&str],
        full: bool,
        keep_open: bool,
        now: Instant,
    ) -> Vec<NvidiaReading> {
        if !self.initialize(now) {
            return vec![NvidiaReading::default(); bus_ids.len()];
        }
        let values = bus_ids
            .iter()
            .map(|bus_id| self.reading(bus_id, full))
            .collect();
        if !keep_open {
            self.release();
        }
        values
    }

    fn release(&mut self) {
        if self.initialized {
            if let Some(library) = &self.library {
                library.shutdown();
            }
            self.initialized = false;
        }
    }

    /// `nvmlShutdown` alone frees nothing; closing the library frees what
    /// NVML allocated (about 20 MiB). The CUDA library it loaded stays mapped,
    /// with its thread, so loading NVML again is about as cheap as
    /// initializing it again.
    fn unload(&mut self) {
        self.release();
        self.library = None;
    }
}

impl Drop for NvmlReader {
    fn drop(&mut self) {
        self.release();
    }
}

/// `0000:01:00.0` -> `00000000:01:00.0`, for NVML versions that only accept
/// the 8-digit domain; `None` when the id already has one or is malformed.
fn padded_bus_id(bus_id: &str) -> Option<String> {
    let (domain, rest) = bus_id.split_once(':')?;
    let valid = !domain.is_empty()
        && domain.len() < 8
        && domain.chars().all(|c| c.is_ascii_hexdigit())
        && !rest.is_empty();
    valid.then(|| format!("{domain:0>8}:{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_bus_ids_pad_the_domain_to_eight_hex_digits() {
        assert_eq!(
            padded_bus_id("0000:01:00.0").as_deref(),
            Some("00000000:01:00.0")
        );
        assert_eq!(
            padded_bus_id("1:65:00.0").as_deref(),
            Some("00000001:65:00.0")
        );
        assert_eq!(padded_bus_id("00000000:01:00.0"), None, "already padded");
        assert_eq!(padded_bus_id("zz:01:00.0"), None);
        assert_eq!(padded_bus_id("0000"), None);
        assert_eq!(padded_bus_id(""), None);
    }

    #[test]
    fn failures_are_retried_after_sixty_seconds() {
        let start = Instant::now();
        let mut gate = RetryGate::default();
        assert!(gate.ready(start));

        gate.failed(start);
        assert!(!gate.ready(start + Duration::from_secs(59)));
        assert!(gate.ready(start + RETRY_INTERVAL));

        gate.succeeded();
        assert!(gate.ready(start));
    }

    #[test]
    fn a_reader_that_cannot_initialize_reports_every_gpu_unavailable() {
        let start = Instant::now();
        let mut reader = NvmlReader::default();
        reader.retry.failed(start - Duration::from_secs(1));

        assert_eq!(
            reader.read(&["0000:01:00.0", "0000:02:00.0"], true, true, start),
            [NvidiaReading::default(); 2]
        );
        assert!(
            reader.library.is_none(),
            "no load attempt before the retry time"
        );
    }
}
