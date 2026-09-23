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

use super::temperature::NvidiaSource;

const LIBRARY: &CStr = c"libnvidia-ml.so.1";
/// A missing library or a failed initialization is retried this often.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

const NVML_SUCCESS: c_int = 0;
const NVML_ERROR_INVALID_ARGUMENT: c_int = 2;
const NVML_ERROR_NOT_FOUND: c_int = 6;
/// `NVML_TEMPERATURE_GPU` of `nvmlTemperatureSensors_t`: the GPU die.
const NVML_TEMPERATURE_GPU: c_int = 0;

/// `nvmlDevice_t`, an opaque pointer.
type Device = *mut c_void;
type InitFn = unsafe extern "C" fn() -> c_int;
type ShutdownFn = unsafe extern "C" fn() -> c_int;
type HandleByBusIdFn = unsafe extern "C" fn(*const c_char, *mut Device) -> c_int;
type TemperatureFn = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> c_int;

/// The loaded library and the four entry points `tuxctl` uses.
struct Library {
    handle: *mut c_void,
    init: InitFn,
    shutdown: ShutdownFn,
    handle_by_bus_id: HandleByBusIdFn,
    temperature: TemperatureFn,
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
        // const char *, nvmlDevice_t *)` and `nvmlReturn_t
        // nvmlDeviceGetTemperature(nvmlDevice_t, nvmlTemperatureSensors_t,
        // unsigned int *)`, where `nvmlReturn_t` and the sensor enum are C
        // enums (int). The pointers stay valid until `dlclose` in `Drop`.
        unsafe {
            Some(Self {
                handle,
                init: std::mem::transmute::<*mut c_void, InitFn>(init),
                shutdown: std::mem::transmute::<*mut c_void, ShutdownFn>(shutdown),
                handle_by_bus_id: std::mem::transmute::<*mut c_void, HandleByBusIdFn>(
                    handle_by_bus_id,
                ),
                temperature: std::mem::transmute::<*mut c_void, TemperatureFn>(temperature),
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

    /// Only valid between [`Self::init`] and [`Self::shutdown`].
    fn temperature(&self, bus_id: &str) -> Result<i64, c_int> {
        let bus_id = CString::new(bus_id).map_err(|_| NVML_ERROR_INVALID_ARGUMENT)?;
        let mut device: Device = ptr::null_mut();
        // SAFETY: NVML is initialized (caller contract), `bus_id` is
        // NUL-terminated and `device` is a writable handle slot.
        let status = unsafe { (self.handle_by_bus_id)(bus_id.as_ptr(), &mut device) };
        if status != NVML_SUCCESS {
            return Err(status);
        }
        let mut celsius: c_uint = 0;
        // SAFETY: `device` was just returned by NVML for this initialization
        // and `celsius` is a writable `unsigned int`.
        let status = unsafe { (self.temperature)(device, NVML_TEMPERATURE_GPU, &mut celsius) };
        if status == NVML_SUCCESS {
            Ok(i64::from(celsius))
        } else {
            Err(status)
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

    fn temperature(&mut self, bus_id: &str) -> Option<i64> {
        let library = self.library.as_ref()?;
        let padded = padded_bus_id(bus_id);
        if let Some(padded) = padded
            .as_deref()
            .filter(|padded| self.padded.iter().any(|known| known == padded))
        {
            return library.temperature(padded).ok();
        }
        match library.temperature(bus_id) {
            Ok(celsius) => Some(celsius),
            Err(NVML_ERROR_INVALID_ARGUMENT | NVML_ERROR_NOT_FOUND) => {
                let padded = padded?;
                let celsius = library.temperature(&padded).ok()?;
                self.padded.push(padded);
                Some(celsius)
            }
            Err(_) => None,
        }
    }
}

impl NvidiaSource for NvmlReader {
    fn read(&mut self, bus_ids: &[&str], keep_open: bool, now: Instant) -> Vec<Option<i64>> {
        if !self.initialize(now) {
            return vec![None; bus_ids.len()];
        }
        let values = bus_ids
            .iter()
            .map(|bus_id| self.temperature(bus_id))
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
            reader.read(&["0000:01:00.0", "0000:02:00.0"], true, start),
            [None, None]
        );
        assert!(
            reader.library.is_none(),
            "no load attempt before the retry time"
        );
    }
}
