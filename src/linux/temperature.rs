//! Component temperatures from hwmon and thermal zones, plus NVIDIA GPUs of
//! the proprietary driver through an [`NvidiaSource`]. Discovery and reads run
//! on the metrics worker only, never on the render path.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    gpu::{self, GpuTelemetry},
    hardware::{is_whole_disk, read_sorted_directories, read_trimmed, sorted_drm_cards},
    system::ByteUsage,
};

/// Temperatures are read at most this often, whatever the sampling interval.
pub(super) const READ_INTERVAL: Duration = Duration::from_secs(2);
/// A sample this much early still counts as due, so timer jitter at a 2 s
/// interval cannot skip every other read. Shorter than the shortest sampling
/// preset (250 ms), so faster intervals still read every 2 s, never earlier.
const READ_TOLERANCE: Duration = Duration::from_millis(100);
/// Sensors that stop reading (hotplug, CPU offlined, driver reload) trigger a
/// new discovery at most this often.
const REDISCOVERY_INTERVAL: Duration = Duration::from_secs(30);
/// NVML that stays open is unloaded once nothing has shown its values for
/// this long, and loaded again when they are shown.
pub(super) const NVML_RELEASE_GRACE: Duration = Duration::from_secs(10);
/// Readings outside this range are treated as invalid rather than shown.
const VALID_CELSIUS: std::ops::RangeInclusive<i64> = -40..=150;

/// What a temperature belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TemperatureKey {
    /// A CPU package by its `physical id` (0 when /proc/cpuinfo has none).
    CpuPackage(u32),
    /// A device by its canonical sysfs path, as stored in the hardware inventory.
    Device(Arc<Path>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Temperature {
    pub key: TemperatureKey,
    /// `None`: the sensor exists but has no valid value right now (a
    /// suspended GPU, NVML unavailable, an invalid or failed reading).
    pub celsius: Option<i16>,
    /// Limits reported by the driver; never invented.
    pub max: Option<i16>,
    pub crit: Option<i16>,
}

impl Temperature {
    /// The limit the driver reports: the critical temperature, else the
    /// maximum. Never invented here.
    pub fn limit(&self) -> Option<i16> {
        self.crit.or(self.max)
    }
}

/// Reads NVIDIA GPU temperatures (NVML). A seam so the sampler's decisions
/// can be tested without the library or the hardware.
/// What NVML reports for one GPU; each value `None` where unavailable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct NvidiaReading {
    /// Degrees Celsius.
    pub temperature: Option<i64>,
    /// The temperature at which the GPU starts to slow itself down.
    pub slowdown: Option<i64>,
    /// Busy percentage of the graphics engine.
    pub utilization: Option<u32>,
    /// Used and total bytes of video memory.
    pub memory: Option<(u64, u64)>,
    pub power_milliwatts: Option<u32>,
    pub fan_percent: Option<u32>,
}

pub(super) trait NvidiaSource {
    /// A reading for each PCI bus id. Without `full`, only the cheap values
    /// (utilization and power) are read: the temperature, memory and fan
    /// queries cost up to a millisecond. With `keep_open` false nothing may
    /// stay initialized after the call, so the GPUs can runtime-suspend.
    fn read(
        &mut self,
        bus_ids: &[&str],
        full: bool,
        keep_open: bool,
        now: Instant,
    ) -> Vec<NvidiaReading>;
    /// Drops any open session.
    fn release(&mut self);
    /// Drops any open session and whatever the library holds; the next read
    /// loads it again.
    fn unload(&mut self) {
        self.release();
    }
}

/// For builds without NVML: NVIDIA GPUs are known but have no temperature.
#[cfg(any(test, target_env = "musl"))]
#[derive(Debug, Default)]
pub(super) struct NoNvidia;

#[cfg(any(test, target_env = "musl"))]
impl NvidiaSource for NoNvidia {
    fn read(
        &mut self,
        bus_ids: &[&str],
        _full: bool,
        _keep_open: bool,
        _now: Instant,
    ) -> Vec<NvidiaReading> {
        vec![NvidiaReading::default(); bus_ids.len()]
    }

    fn release(&mut self) {}
}

/// Where discovery looks; tests point these at fake trees.
#[derive(Debug, Clone)]
pub(super) struct SysfsRoots {
    pub hwmon: PathBuf,
    pub thermal: PathBuf,
    pub drm: PathBuf,
    pub block: PathBuf,
    pub net: PathBuf,
    pub nvidia_proc: PathBuf,
    pub powercap: PathBuf,
}

impl Default for SysfsRoots {
    fn default() -> Self {
        Self {
            hwmon: "/sys/class/hwmon".into(),
            thermal: "/sys/class/thermal".into(),
            drm: "/sys/class/drm".into(),
            block: "/sys/block".into(),
            net: "/sys/class/net".into(),
            nvidia_proc: "/proc/driver/nvidia/gpus".into(),
            powercap: "/sys/class/powercap".into(),
        }
    }
}

/// Where one temperature comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// The maximum of the valid values in these files (millidegrees).
    Files(Vec<PathBuf>),
    /// A GPU's own hwmon sensor, read only while the GPU is awake.
    GpuFile {
        input: PathBuf,
        runtime_status: PathBuf,
    },
    /// A GPU of the proprietary NVIDIA driver, read through NVML.
    Nvidia {
        bus_id: String,
        runtime_status: PathBuf,
        /// NVML may stay initialized: the GPU cannot runtime-suspend anyway.
        keep_open: bool,
    },
}

/// Where a GPU's utilization, memory, power and fan come from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GpuSource {
    /// Kernel drivers: sysfs and the GPU's own hwmon directory, read only
    /// while the GPU is awake.
    Sysfs {
        path: Arc<Path>,
        runtime_status: PathBuf,
        hwmon: Option<PathBuf>,
    },
    /// The proprietary NVIDIA driver: read with the GPU's NVML temperature.
    Nvidia { path: Arc<Path> },
}

impl GpuSource {
    fn path(&self) -> &Arc<Path> {
        match self {
            Self::Sysfs { path, .. } | Self::Nvidia { path } => path,
        }
    }
}

/// Everything discovery finds.
#[derive(Debug, Default)]
struct Discovery {
    sensors: Vec<Sensor>,
    gpus: Vec<GpuSource>,
    cpu_power: CpuPowerSource,
}

/// Where CPU package power comes from, if anywhere this user can read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum CpuPowerSource {
    #[default]
    None,
    /// `power*_input` files of the CPU's hwmon, microwatts (zenpower's core
    /// and SoC power), summed.
    Hwmon(Vec<PathBuf>),
    /// RAPL package domains: their energy counters (microjoules) and the
    /// value at which each wraps. Root-only unless an administrator made
    /// them readable.
    Rapl(Vec<(PathBuf, u64)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sensor {
    key: TemperatureKey,
    source: Source,
    max: Option<i16>,
    crit: Option<i16>,
    /// Where the value comes from, for `tuxctl --check`: `k10temp Tctl`.
    origin: String,
}

pub(super) struct TemperatureSampler<N> {
    roots: SysfsRoots,
    sensors: Vec<Sensor>,
    /// Whether each sensor has read successfully at least once.
    worked: Vec<bool>,
    readings: Vec<Temperature>,
    last_read: Option<Instant>,
    last_discovery: Instant,
    rediscover: bool,
    buffer: String,
    /// `None`: NVIDIA temperatures are off; NVIDIA GPUs get no entry.
    nvidia: Option<N>,
    gpu_sources: Vec<GpuSource>,
    /// One entry per GPU source, in the same order.
    gpus: Vec<GpuTelemetry>,
    cpu_power: CpuPowerSource,
    /// The previous RAPL counters and when they were read.
    rapl_previous: Option<(Instant, Vec<u64>)>,
    cpu_power_watts: Option<f64>,
    /// While false (nothing shows the values), nothing is read and the last
    /// values are kept.
    active: bool,
    /// When the sampler became inactive while NVML may be open; cleared once
    /// NVML is unloaded.
    inactive_since: Option<Instant>,
}

impl<N: NvidiaSource> TemperatureSampler<N> {
    pub(super) fn new(roots: SysfsRoots, nvidia: Option<N>, now: Instant) -> Self {
        let mut sampler = Self {
            roots,
            sensors: Vec::new(),
            worked: Vec::new(),
            readings: Vec::new(),
            last_read: None,
            last_discovery: now,
            rediscover: false,
            buffer: String::new(),
            nvidia,
            gpu_sources: Vec::new(),
            gpus: Vec::new(),
            cpu_power: CpuPowerSource::None,
            rapl_previous: None,
            cpu_power_watts: None,
            active: true,
            inactive_since: None,
        };
        sampler.discover(now);
        sampler
    }

    /// The current temperatures, read from the sensors at most once per
    /// [`READ_INTERVAL`] and carried over unchanged in between. GPU telemetry
    /// is refreshed on every call where that is cheap: kernel drivers, and
    /// NVML while it stays open; NVML initialized per reading (RTD3) follows
    /// the temperature cadence.
    pub(super) fn sample(&mut self, now: Instant) -> &[Temperature] {
        if !self.active {
            if self
                .inactive_since
                .is_some_and(|since| now.saturating_duration_since(since) >= NVML_RELEASE_GRACE)
            {
                self.inactive_since = None;
                if let Some(nvidia) = &mut self.nvidia {
                    nvidia.unload();
                }
            }
            return &self.readings;
        }
        let due = self.last_read.is_none_or(|last| {
            now.saturating_duration_since(last) + READ_TOLERANCE >= READ_INTERVAL
        });
        if due {
            self.last_read = Some(now);
            if self.rediscover
                && now.saturating_duration_since(self.last_discovery) >= REDISCOVERY_INTERVAL
            {
                self.discover(now);
            }
            self.read(now);
            self.cpu_power_watts = self.read_cpu_power(now);
        }
        self.read_nvidia(now, due);
        self.read_gpus();
        &self.readings
    }

    /// Whether [`Self::sample`] reads anything. The first sample after
    /// becoming active reads everything at once, so the values shown are
    /// fresh; a RAPL delta across the pause is not computed. NVML kept open
    /// is unloaded after [`NVML_RELEASE_GRACE`] of inactivity (it holds
    /// about 20 MiB); NVML initialized per reading (RTD3) is already closed.
    pub(super) fn set_active(&mut self, active: bool, now: Instant) {
        if active && !self.active {
            self.last_read = None;
            self.rapl_previous = None;
            self.inactive_since = None;
        } else if !active && self.active && self.nvidia_keeps_open() {
            self.inactive_since = Some(now);
        }
        self.active = active;
    }

    /// Whether NVML stays open between readings: there are NVIDIA GPUs and
    /// none of them can runtime-suspend.
    fn nvidia_keeps_open(&self) -> bool {
        let mut gpus = self
            .sensors
            .iter()
            .filter_map(|sensor| match sensor.source {
                Source::Nvidia { keep_open, .. } => Some(keep_open),
                _ => None,
            })
            .peekable();
        self.nvidia.is_some() && gpus.peek().is_some() && gpus.all(|keep_open| keep_open)
    }

    /// Telemetry of each GPU, as of the last [`Self::sample`].
    pub(super) fn gpus(&self) -> &[GpuTelemetry] {
        &self.gpus
    }

    /// Where each temperature comes from, such as `k10temp Tctl` or `NVML`.
    pub(super) fn origin(&self, key: &TemperatureKey) -> Option<&str> {
        self.sensors
            .iter()
            .find(|sensor| sensor.key == *key)
            .map(|sensor| sensor.origin.as_str())
    }

    /// Where CPU package power is read from, if anywhere.
    pub(super) fn cpu_power_origin(&self) -> Option<&'static str> {
        match self.cpu_power {
            CpuPowerSource::None => None,
            CpuPowerSource::Hwmon(_) => Some("zenpower"),
            CpuPowerSource::Rapl(_) => Some("RAPL"),
        }
    }

    /// CPU package power, where this user can read it.
    pub(super) fn cpu_power_watts(&self) -> Option<f64> {
        self.cpu_power_watts
    }

    fn discover(&mut self, now: Instant) {
        let discovery = discover(&self.roots, self.nvidia.is_some());
        self.gpus = discovery
            .gpus
            .iter()
            .map(|source| GpuTelemetry::unavailable(Arc::clone(source.path())))
            .collect();
        self.gpu_sources = discovery.gpus;
        self.cpu_power = discovery.cpu_power;
        self.rapl_previous = None;
        self.sensors = discovery.sensors;
        self.worked = vec![false; self.sensors.len()];
        self.readings = self
            .sensors
            .iter()
            .map(|sensor| Temperature {
                key: sensor.key.clone(),
                celsius: None,
                max: sensor.max,
                crit: sensor.crit,
            })
            .collect();
        self.last_discovery = now;
        self.rediscover = false;
    }

    fn read(&mut self, _now: Instant) {
        for (index, sensor) in self.sensors.iter().enumerate() {
            let result = match &sensor.source {
                Source::Files(paths) => read_max(paths, &mut self.buffer),
                Source::GpuFile {
                    input,
                    runtime_status,
                } => {
                    if is_awake(runtime_status) {
                        read_max(std::slice::from_ref(input), &mut self.buffer)
                            // A GPU that suspended between the check and the read.
                            .or_else(|error| {
                                if is_awake(runtime_status) {
                                    Err(error)
                                } else {
                                    Ok(None)
                                }
                            })
                    } else {
                        Ok(None)
                    }
                }
                // Read by `read_nvidia`.
                Source::Nvidia { .. } => continue,
            };
            self.readings[index].celsius = match result {
                Ok(celsius) => {
                    self.worked[index] = true;
                    celsius
                }
                Err(_) => {
                    self.rediscover |= self.worked[index];
                    None
                }
            };
        }
    }

    /// NVIDIA GPUs through NVML: utilization and power on every call while
    /// NVML stays open; temperatures, memory and fan when `due`. NVML
    /// initialized per reading is read only when `due`.
    fn read_nvidia(&mut self, now: Instant, due: bool) {
        let nvidia: Vec<(usize, &str, bool, bool)> = self
            .sensors
            .iter()
            .enumerate()
            .filter_map(|(index, sensor)| match &sensor.source {
                Source::Nvidia {
                    bus_id,
                    runtime_status,
                    keep_open,
                } => Some((index, bus_id.as_str(), is_awake(runtime_status), *keep_open)),
                _ => None,
            })
            .collect();
        // Only discovered while NVIDIA temperatures are on.
        let Some(source) = self.nvidia.as_mut().filter(|_| !nvidia.is_empty()) else {
            return;
        };
        // NVML attaches to every NVIDIA GPU on init. Keep it open only when
        // none of them can runtime-suspend; otherwise initialize it for one
        // read, and only while all of them are awake, so it never wakes one.
        let keep_open = nvidia.iter().all(|&(.., keep_open)| keep_open);
        let all_awake = nvidia.iter().all(|&(_, _, awake, _)| awake);
        if !keep_open && !due {
            return;
        }
        let readings: Vec<NvidiaReading> = if !keep_open && !all_awake {
            source.release();
            vec![NvidiaReading::default(); nvidia.len()]
        } else {
            let bus_ids: Vec<&str> = nvidia
                .iter()
                .filter(|&&(_, _, awake, _)| awake)
                .map(|&(_, bus_id, ..)| bus_id)
                .collect();
            let mut values = if bus_ids.is_empty() {
                Vec::new()
            } else {
                source.read(&bus_ids, due, keep_open, now)
            }
            .into_iter();
            nvidia
                .iter()
                .map(|&(_, _, awake, _)| {
                    if awake {
                        values.next().unwrap_or_default()
                    } else {
                        NvidiaReading::default()
                    }
                })
                .collect()
        };
        for (&(index, ..), reading) in nvidia.iter().zip(readings) {
            if due {
                let temperature = &mut self.readings[index];
                temperature.celsius = reading.temperature.and_then(valid_celsius);
                if temperature.max.is_none() {
                    temperature.max = reading.slowdown.and_then(valid_celsius);
                }
            }
            let TemperatureKey::Device(path) = &self.sensors[index].key else {
                continue;
            };
            if let Some(slot) = self.gpus.iter_mut().find(|gpu| gpu.device_path == *path) {
                let telemetry = nvidia_telemetry(Arc::clone(path), reading);
                // Memory and fan are read with the temperatures; in between
                // they carry over.
                *slot = if due {
                    telemetry
                } else {
                    GpuTelemetry {
                        vram: slot.vram,
                        fan_percent: slot.fan_percent,
                        ..telemetry
                    }
                };
            }
        }
    }

    /// Package power in watts: zenpower's reading, or the RAPL energy used
    /// since the previous call (none on the first).
    fn read_cpu_power(&mut self, now: Instant) -> Option<f64> {
        let watts = match &self.cpu_power {
            CpuPowerSource::None => return None,
            CpuPowerSource::Hwmon(files) => {
                let mut microwatts = 0.0;
                for file in files {
                    microwatts += fs::read_to_string(file).ok()?.trim().parse::<f64>().ok()?;
                }
                microwatts / 1_000_000.0
            }
            CpuPowerSource::Rapl(domains) => {
                let energies: Option<Vec<u64>> = domains
                    .iter()
                    .map(|(file, _)| fs::read_to_string(file).ok()?.trim().parse().ok())
                    .collect();
                let previous = match energies {
                    Some(energies) => self.rapl_previous.replace((now, energies)),
                    None => self.rapl_previous.take(),
                };
                let ((then, before), (_, current)) = (previous?, self.rapl_previous.as_ref()?);
                let seconds = now.saturating_duration_since(then).as_secs_f64();
                if seconds < 0.1 {
                    return None;
                }
                let mut microjoules = 0_u64;
                for ((&after, &before), &(_, range)) in current.iter().zip(&before).zip(domains) {
                    // The counter wraps at `range`.
                    let delta = if after >= before {
                        after - before
                    } else {
                        range.checked_sub(before)?.checked_add(after)?
                    };
                    microjoules = microjoules.checked_add(delta)?;
                }
                microjoules as f64 / 1_000_000.0 / seconds
            }
        };
        (0.0..=2_000.0).contains(&watts).then_some(watts)
    }

    /// GPUs of kernel drivers, while they are awake.
    fn read_gpus(&mut self) {
        for (source, slot) in self.gpu_sources.iter().zip(&mut self.gpus) {
            if let GpuSource::Sysfs {
                path,
                runtime_status,
                hwmon,
            } = source
            {
                *slot = if is_awake(runtime_status) {
                    gpu::read_sysfs(path, hwmon.as_deref())
                } else {
                    GpuTelemetry::unavailable(Arc::clone(path))
                };
            }
        }
    }
}

fn nvidia_telemetry(device_path: Arc<Path>, reading: NvidiaReading) -> GpuTelemetry {
    GpuTelemetry {
        device_path,
        utilization: reading
            .utilization
            .map(|percent| f64::from(percent.min(100))),
        vram: reading
            .memory
            .filter(|&(_, total)| total > 0)
            .map(|(used, total)| ByteUsage { used, total }),
        power_watts: reading
            .power_milliwatts
            .map(|milliwatts| f64::from(milliwatts) / 1000.0)
            .filter(|watts| *watts <= 2_000.0),
        fan_percent: reading
            .fan_percent
            .map(|percent| f64::from(percent.min(100))),
    }
}

/// A device that is not runtime-suspended; reading its sensor does not wake it.
/// No `runtime_status` file means no runtime power management.
fn is_awake(runtime_status: &Path) -> bool {
    read_trimmed(runtime_status)
        .is_none_or(|status| matches!(status.as_str(), "active" | "unsupported"))
}

/// The maximum valid value among `paths`; an I/O error on any of them is a
/// failure, an unparsable or out-of-range value is not.
fn read_max(paths: &[PathBuf], buffer: &mut String) -> io::Result<Option<i16>> {
    let mut maximum = None;
    let mut failure = None;
    for path in paths {
        buffer.clear();
        match fs::File::open(path).and_then(|mut file| file.read_to_string(buffer)) {
            Ok(_) => {
                let value = buffer
                    .trim()
                    .parse()
                    .ok()
                    .and_then(celsius_from_millidegrees);
                maximum = maximum.max(value);
            }
            Err(error) => failure = Some(error),
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(maximum),
    }
}

/// Millidegrees to whole degrees, rounded half away from zero; `None` when
/// outside the plausible range.
fn celsius_from_millidegrees(millidegrees: i64) -> Option<i16> {
    let offset = if millidegrees < 0 { -500 } else { 500 };
    valid_celsius(millidegrees.saturating_add(offset) / 1000)
}

fn valid_celsius(celsius: i64) -> Option<i16> {
    VALID_CELSIUS
        .contains(&celsius)
        .then(|| i16::try_from(celsius).ok())
        .flatten()
}

#[derive(Debug)]
struct Hwmon {
    dir: PathBuf,
    name: String,
    device: Option<PathBuf>,
    temps: Vec<HwmonTemp>,
    /// `power*_input` files, microwatts.
    powers: Vec<PathBuf>,
}

#[derive(Debug)]
struct HwmonTemp {
    index: u32,
    input: PathBuf,
    label: Option<String>,
    max: Option<i16>,
    crit: Option<i16>,
}

impl Hwmon {
    fn labeled(&self, label: &str) -> Option<&HwmonTemp> {
        self.temps
            .iter()
            .find(|temp| temp.label.as_deref() == Some(label))
    }

    fn sensor(&self, key: TemperatureKey, temp: &HwmonTemp) -> Sensor {
        Sensor {
            key,
            source: Source::Files(vec![temp.input.clone()]),
            max: temp.max,
            crit: temp.crit,
            origin: self.origin(temp),
        }
    }

    /// `k10temp Tctl`, or `nvme temp1` for an unlabeled input.
    fn origin(&self, temp: &HwmonTemp) -> String {
        match &temp.label {
            Some(label) => format!("{} {label}", self.name),
            None => format!("{} temp{}", self.name, temp.index),
        }
    }
}

struct Gpu {
    path: PathBuf,
    driver: Option<String>,
}

fn discover(roots: &SysfsRoots, nvidia: bool) -> Discovery {
    let hwmons = read_hwmons(&roots.hwmon);
    let mut sensors = cpu_sensors(&hwmons);
    if sensors.is_empty() {
        sensors.extend(thermal_zone_sensor(&roots.thermal));
    }

    let gpus = read_gpus(&roots.drm);
    let disks = read_devices(&roots.block, is_whole_disk);
    let nics = read_devices(&roots.net, |_| true);
    for hwmon in &hwmons {
        let Some(device) = &hwmon.device else {
            continue;
        };
        sensors.extend(device_sensors(hwmon, device, &gpus, &disks, &nics));
    }
    for gpu in gpus
        .iter()
        .filter(|gpu| nvidia && gpu.driver.as_deref() == Some("nvidia"))
    {
        if let Some(bus_id) = gpu.path.file_name().and_then(|name| name.to_str()) {
            let proc_power = fs::read_to_string(roots.nvidia_proc.join(bus_id).join("power")).ok();
            sensors.push(Sensor {
                key: TemperatureKey::Device(Arc::from(gpu.path.as_path())),
                source: Source::Nvidia {
                    bus_id: bus_id.to_owned(),
                    runtime_status: gpu.path.join("power/runtime_status"),
                    keep_open: nvidia_keeps_open(
                        read_trimmed(gpu.path.join("power/control")).as_deref(),
                        proc_power.as_deref(),
                    ),
                },
                max: None,
                crit: None,
                origin: "NVML".into(),
            });
        }
    }

    // One temperature per component: the first source found wins.
    let mut seen = std::collections::HashSet::new();
    sensors.retain(|sensor| seen.insert(sensor.key.clone()));

    let gpu_sources = gpus
        .iter()
        .filter_map(|gpu| {
            let path: Arc<Path> = Arc::from(gpu.path.as_path());
            if gpu.driver.as_deref() == Some("nvidia") {
                return nvidia.then_some(GpuSource::Nvidia { path });
            }
            Some(GpuSource::Sysfs {
                runtime_status: gpu.path.join("power/runtime_status"),
                hwmon: hwmons
                    .iter()
                    .find(|hwmon| hwmon.device.as_deref() == Some(gpu.path.as_path()))
                    .map(|hwmon| hwmon.dir.clone()),
                path,
            })
        })
        .collect();
    // zenpower reports core and SoC power; their sum stands for the package.
    let zenpower: Vec<PathBuf> = hwmons
        .iter()
        .filter(|hwmon| hwmon.name == "zenpower")
        .flat_map(|hwmon| hwmon.powers.iter().cloned())
        .collect();
    let cpu_power = if zenpower.is_empty() {
        let domains = readable_rapl_packages(&roots.powercap);
        if domains.is_empty() {
            CpuPowerSource::None
        } else {
            CpuPowerSource::Rapl(domains)
        }
    } else {
        CpuPowerSource::Hwmon(zenpower)
    };
    Discovery {
        sensors,
        gpus: gpu_sources,
        cpu_power,
    }
}

/// RAPL package domains (`intel-rapl:N` named `package-*`, also on AMD)
/// whose energy counter this user can read. Their subdomains (`intel-rapl:N:M`)
/// are parts of the package and not added.
/// Whether the machine has RAPL domains at all, readable or not.
pub(super) fn rapl_present(root: &Path) -> bool {
    !read_sorted_directories(root, |name| name.starts_with("intel-rapl:")).is_empty()
}

fn readable_rapl_packages(root: &Path) -> Vec<(PathBuf, u64)> {
    read_sorted_directories(root, |name| {
        name.strip_prefix("intel-rapl:")
            .is_some_and(|index| !index.is_empty() && index.chars().all(|c| c.is_ascii_digit()))
    })
    .into_iter()
    .filter(|domain| {
        read_trimmed(domain.join("name")).is_some_and(|name| name.starts_with("package"))
    })
    .filter_map(|domain| {
        let energy = domain.join("energy_uj");
        fs::read_to_string(&energy)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        let range = read_trimmed(domain.join("max_energy_range_uj"))
            .and_then(|range| range.parse().ok())
            .unwrap_or(u64::MAX);
        Some((energy, range))
    })
    .collect()
}

fn read_hwmons(root: &Path) -> Vec<Hwmon> {
    let mut hwmons: Vec<(u32, Hwmon)> =
        read_sorted_directories(root, |name| hwmon_index(name).is_some())
            .into_iter()
            .filter_map(|path| {
                let index = hwmon_index(path.file_name()?.to_str()?)?;
                let name = read_trimmed(path.join("name"))?;
                let device = fs::canonicalize(path.join("device")).ok();
                let mut temps: Vec<HwmonTemp> = sorted_file_names(&path)
                    .into_iter()
                    .filter_map(|file| {
                        let index = temp_input_index(&file)?;
                        let limit = |suffix: &str| {
                            read_trimmed(path.join(format!("temp{index}_{suffix}")))
                                .and_then(|value| value.parse().ok())
                                .and_then(celsius_from_millidegrees)
                        };
                        Some(HwmonTemp {
                            index,
                            input: path.join(&file),
                            label: read_trimmed(path.join(format!("temp{index}_label"))),
                            max: limit("max"),
                            crit: limit("crit"),
                        })
                    })
                    .collect();
                temps.sort_by_key(|temp| temp.index);
                let powers = sorted_file_names(&path)
                    .into_iter()
                    .filter(|file| {
                        file.strip_prefix("power")
                            .and_then(|rest| rest.strip_suffix("_input"))
                            .is_some_and(|number| number.parse::<u32>().is_ok())
                    })
                    .map(|file| path.join(file))
                    .collect();
                Some((
                    index,
                    Hwmon {
                        dir: path.clone(),
                        name,
                        device,
                        temps,
                        powers,
                    },
                ))
            })
            .collect();
    hwmons.sort_by_key(|(index, _)| *index);
    hwmons.into_iter().map(|(_, hwmon)| hwmon).collect()
}

/// File names in `directory`, sorted.
fn sorted_file_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

fn hwmon_index(name: &str) -> Option<u32> {
    name.strip_prefix("hwmon")?.parse().ok()
}

fn temp_input_index(file: &str) -> Option<u32> {
    file.strip_prefix("temp")?
        .strip_suffix("_input")?
        .parse()
        .ok()
}

fn cpu_sensors(hwmons: &[Hwmon]) -> Vec<Sensor> {
    let mut sensors = Vec::new();
    for hwmon in hwmons.iter().filter(|hwmon| hwmon.name == "coretemp") {
        let packages: Vec<_> = hwmon
            .temps
            .iter()
            .filter_map(|temp| {
                let id = temp.label.as_deref()?.strip_prefix("Package id ")?;
                Some((id.trim().parse::<u32>().ok()?, temp))
            })
            .collect();
        if packages.is_empty() {
            // Without a package sensor, the hottest core stands for the package,
            // whose id is the platform device's (`coretemp.N`).
            let package = hwmon
                .device
                .as_deref()
                .and_then(|device| {
                    device
                        .file_name()?
                        .to_str()?
                        .strip_prefix("coretemp.")?
                        .parse()
                        .ok()
                })
                .unwrap_or(0);
            sensors.extend(core_maximum(&hwmon.name, hwmon.temps.iter(), package));
        } else {
            for (id, temp) in packages {
                sensors.push(hwmon.sensor(TemperatureKey::CpuPackage(id), temp));
            }
        }
    }

    // VIA CPUs are single-socket: all their core sensors form package 0.
    sensors.extend(core_maximum(
        "via_cputemp",
        hwmons
            .iter()
            .filter(|hwmon| hwmon.name == "via_cputemp")
            .flat_map(|hwmon| &hwmon.temps),
        0,
    ));

    // One k10temp/zenpower instance per node (PCI 00:18.3, 00:19.3, …). Sorted
    // by device path, the i-th is taken to be the package with `physical id` i,
    // which holds for the usual one node per socket.
    let mut amd: Vec<&Hwmon> = hwmons
        .iter()
        .filter(|hwmon| matches!(hwmon.name.as_str(), "k10temp" | "zenpower"))
        .collect();
    amd.sort_by(|left, right| match (&left.device, &right.device) {
        (Some(left), Some(right)) => left.cmp(right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    for (package, hwmon) in (0_u32..).zip(amd) {
        // Tccd* are per-die sensors, not the package temperature. Old kernels
        // label nothing; their temp1 is Tctl.
        let temp = hwmon
            .labeled("Tdie")
            .or_else(|| hwmon.labeled("Tctl"))
            .or_else(|| {
                hwmon
                    .temps
                    .iter()
                    .find(|temp| temp.index == 1 && temp.label.is_none())
            });
        if let Some(temp) = temp {
            sensors.push(hwmon.sensor(TemperatureKey::CpuPackage(package), temp));
        }
    }
    sensors
}

/// One sensor reporting the hottest of `temps` for `package`.
fn core_maximum<'a>(
    driver: &str,
    temps: impl Iterator<Item = &'a HwmonTemp>,
    package: u32,
) -> Option<Sensor> {
    let cores: Vec<&HwmonTemp> = temps
        .filter(|temp| {
            temp.label
                .as_deref()
                .is_none_or(|label| label.starts_with("Core"))
        })
        .collect();
    if cores.is_empty() {
        return None;
    }
    Some(Sensor {
        key: TemperatureKey::CpuPackage(package),
        source: Source::Files(cores.iter().map(|temp| temp.input.clone()).collect()),
        max: cores.iter().filter_map(|temp| temp.max).min(),
        crit: cores.iter().filter_map(|temp| temp.crit).min(),
        origin: format!("{driver} (hottest core)"),
    })
}

/// Fallback for CPUs without a hwmon driver (ARM and other SoCs): the hottest
/// thermal zone whose type names the CPU or SoC. `acpitz` and other zones of
/// unknown placement are never taken for the CPU.
fn thermal_zone_sensor(root: &Path) -> Option<Sensor> {
    let files: Vec<PathBuf> =
        read_sorted_directories(root, |name| name.starts_with("thermal_zone"))
            .into_iter()
            .filter(|zone| read_trimmed(zone.join("type")).is_some_and(|kind| is_cpu_zone(&kind)))
            .map(|zone| zone.join("temp"))
            .collect();
    if files.is_empty() {
        return None;
    }
    Some(Sensor {
        key: TemperatureKey::CpuPackage(0),
        source: Source::Files(files),
        max: None,
        crit: None,
        origin: "thermal zones".into(),
    })
}

fn is_cpu_zone(kind: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    kind == "x86_pkg_temp" || kind.contains("cpu") || kind.contains("soc")
}

fn read_gpus(drm_root: &Path) -> Vec<Gpu> {
    let names = fs::read_dir(drm_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned));
    sorted_drm_cards(names)
        .into_iter()
        .filter_map(|(_, card)| {
            let device = drm_root.join(card).join("device");
            let path = fs::canonicalize(&device).ok()?;
            let driver = fs::canonicalize(device.join("driver"))
                .ok()
                .and_then(|driver| driver.file_name()?.to_str().map(str::to_owned));
            Some(Gpu { path, driver })
        })
        .collect()
}

/// `(name, canonical device path)` of the entries of `root` that have a device.
fn read_devices(root: &Path, keep: impl Fn(&str) -> bool) -> Vec<(String, PathBuf)> {
    read_sorted_directories(root, keep)
        .into_iter()
        .filter_map(|entry| {
            let name = entry.file_name()?.to_str()?.to_owned();
            Some((name, fs::canonicalize(entry.join("device")).ok()?))
        })
        .collect()
}

/// Sensors of a hwmon attached to a device: a GPU, a disk or a NIC.
fn device_sensors(
    hwmon: &Hwmon,
    device: &Path,
    gpus: &[Gpu],
    disks: &[(String, PathBuf)],
    nics: &[(String, PathBuf)],
) -> Vec<Sensor> {
    let key = |path: &Path| TemperatureKey::Device(Arc::from(path));
    let first = hwmon.temps.first();

    if let Some(gpu) = gpus.iter().find(|gpu| gpu.path == device) {
        let temp = match gpu.driver.as_deref() {
            Some("amdgpu") => hwmon.labeled("edge").or(first),
            Some("radeon" | "nouveau" | "i915" | "xe") => first,
            _ => None,
        };
        return temp
            .map(|temp| Sensor {
                key: key(&gpu.path),
                source: Source::GpuFile {
                    input: temp.input.clone(),
                    runtime_status: gpu.path.join("power/runtime_status"),
                },
                max: temp.max,
                crit: temp.crit,
                origin: hwmon.origin(temp),
            })
            .into_iter()
            .collect();
    }

    match hwmon.name.as_str() {
        "nvme" => {
            let Some(temp) = hwmon.labeled("Composite").or(first) else {
                return Vec::new();
            };
            if disks.iter().any(|(_, path)| path == device) {
                return vec![hwmon.sensor(key(device), temp)];
            }
            // The hwmon may hang off the PCI function instead of the controller,
            // and with native multipath the disk's device is the subsystem, so
            // fall back to the names: controller nvmeX serves nvmeXnY.
            let Some(controller) = nvme_controller(device) else {
                return Vec::new();
            };
            disks
                .iter()
                .filter(|(name, _)| nvme_disk_controller(name) == Some(controller.as_str()))
                .map(|(_, path)| hwmon.sensor(key(path), temp))
                .collect()
        }
        "drivetemp" => disks
            .iter()
            .filter(|(_, path)| path == device)
            .filter_map(|(_, path)| Some(hwmon.sensor(key(path), first?)))
            .collect(),
        _ => nics
            .iter()
            // A NIC's sensor may belong to a child device, such as its PHY.
            .filter(|(_, path)| device.starts_with(path))
            .filter_map(|(_, path)| Some(hwmon.sensor(key(path), first?)))
            .take(1)
            .collect(),
    }
}

fn nvme_controller(device: &Path) -> Option<String> {
    let name = device.file_name()?.to_str()?;
    if is_nvme_controller(name) {
        return Some(name.to_owned());
    }
    read_sorted_directories(&device.join("nvme"), is_nvme_controller)
        .first()?
        .file_name()?
        .to_str()
        .map(str::to_owned)
}

fn is_nvme_controller(name: &str) -> bool {
    name.strip_prefix("nvme")
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

/// `nvme0n1` -> `nvme0`.
fn nvme_disk_controller(disk: &str) -> Option<&str> {
    let (controller, _) = disk.strip_prefix("nvme")?.split_once('n')?;
    Some(&disk[..4 + controller.len()])
}

/// Whether NVML may stay initialized for this GPU. An open NVML handle keeps
/// the GPU out of runtime D3 (RTD3), so it stays open only when the GPU cannot
/// suspend anyway: runtime PM is forced on, or the NVIDIA driver says RTD3 is
/// not supported or disabled. Anything unrecognized takes the power-saving side.
fn nvidia_keeps_open(control: Option<&str>, proc_power: Option<&str>) -> bool {
    if control == Some("on") {
        return true;
    }
    proc_power
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == "Runtime D3 status").then(|| value.trim())
            })
        })
        .is_some_and(|status| status == "Not supported" || status.starts_with("Disabled"))
}

#[cfg(test)]
mod tests;
