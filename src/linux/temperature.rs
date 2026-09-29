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
    /// fresh; a RAPL delta across the pause is not computed.
    pub(super) fn set_active(&mut self, active: bool) {
        if active && !self.active {
            self.last_read = None;
            self.rapl_previous = None;
        }
        self.active = active;
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
mod tests {
    use std::{cell::RefCell, os::unix::fs::symlink, rc::Rc};

    use super::*;

    /// A fake /sys tree under a unique temporary directory.
    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "tuxctl-temperature-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(fs::canonicalize(root).unwrap())
        }

        fn roots(&self) -> SysfsRoots {
            SysfsRoots {
                hwmon: self.0.join("class/hwmon"),
                thermal: self.0.join("class/thermal"),
                drm: self.0.join("class/drm"),
                block: self.0.join("block"),
                net: self.0.join("class/net"),
                nvidia_proc: self.0.join("proc/nvidia/gpus"),
                powercap: self.0.join("class/powercap"),
            }
        }

        fn write(&self, path: &Path, contents: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn device(&self, relative: &str) -> PathBuf {
            let path = self.0.join("devices").join(relative);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn link(&self, target: &Path, link: &Path) {
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(target, link).unwrap();
        }

        /// `temps`: (index, label, millidegrees).
        fn hwmon(
            &self,
            index: u32,
            name: &str,
            device: Option<&Path>,
            temps: &[(u32, Option<&str>, i64)],
        ) -> PathBuf {
            let path = self.0.join(format!("class/hwmon/hwmon{index}"));
            self.write(&path.join("name"), &format!("{name}\n"));
            if let Some(device) = device {
                self.link(device, &path.join("device"));
            }
            for &(temp, label, value) in temps {
                self.write(
                    &path.join(format!("temp{temp}_input")),
                    &format!("{value}\n"),
                );
                if let Some(label) = label {
                    self.write(
                        &path.join(format!("temp{temp}_label")),
                        &format!("{label}\n"),
                    );
                }
            }
            path
        }

        fn gpu(&self, card: u32, device: &Path, driver: &str) {
            let driver_path = self.0.join("drivers").join(driver);
            fs::create_dir_all(&driver_path).unwrap();
            self.link(&driver_path, &device.join("driver"));
            self.link(device, &self.0.join(format!("class/drm/card{card}/device")));
        }

        fn power(&self, device: &Path, control: &str, status: &str) {
            self.write(&device.join("power/control"), control);
            self.write(&device.join("power/runtime_status"), status);
        }

        fn disk(&self, name: &str, device: &Path) {
            self.link(device, &self.0.join(format!("block/{name}/device")));
        }

        fn nic(&self, name: &str, device: &Path) {
            self.link(device, &self.0.join(format!("class/net/{name}/device")));
        }

        fn zone(&self, index: u32, kind: &str, millidegrees: i64) {
            let zone = self.0.join(format!("class/thermal/thermal_zone{index}"));
            self.write(&zone.join("type"), kind);
            self.write(&zone.join("temp"), &millidegrees.to_string());
        }

        fn sampler(&self) -> TemperatureSampler<NoNvidia> {
            TemperatureSampler::new(self.roots(), Some(NoNvidia), Instant::now())
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn device_key(path: &Path) -> TemperatureKey {
        TemperatureKey::Device(Arc::from(path))
    }

    fn values<N: NvidiaSource>(
        sampler: &mut TemperatureSampler<N>,
    ) -> Vec<(TemperatureKey, Option<i16>)> {
        sampler
            .sample(Instant::now())
            .iter()
            .map(|temperature| (temperature.key.clone(), temperature.celsius))
            .collect()
    }

    #[test]
    fn coretemp_packages_map_to_their_physical_ids() {
        let tree = Tree::new("coretemp");
        let package0 = tree.device("platform/coretemp.0");
        let package1 = tree.device("platform/coretemp.1");
        tree.hwmon(
            3,
            "coretemp",
            Some(&package0),
            &[
                (1, Some("Package id 0"), 51_000),
                (2, Some("Core 0"), 49_000),
            ],
        );
        let hwmon = tree.hwmon(
            4,
            "coretemp",
            Some(&package1),
            &[
                (1, Some("Package id 1"), 63_600),
                (2, Some("Core 0"), 60_000),
            ],
        );
        tree.write(&hwmon.join("temp1_max"), "80000");
        tree.write(&hwmon.join("temp1_crit"), "100000");

        let mut sampler = tree.sampler();
        let temperatures = sampler.sample(Instant::now()).to_vec();
        assert_eq!(
            temperatures,
            [
                Temperature {
                    key: TemperatureKey::CpuPackage(0),
                    celsius: Some(51),
                    max: None,
                    crit: None,
                },
                Temperature {
                    key: TemperatureKey::CpuPackage(1),
                    celsius: Some(64),
                    max: Some(80),
                    crit: Some(100),
                },
            ]
        );
    }

    #[test]
    fn coretemp_without_a_package_sensor_uses_its_hottest_core() {
        let tree = Tree::new("coretemp-cores");
        let device = tree.device("platform/coretemp.1");
        tree.hwmon(
            0,
            "coretemp",
            Some(&device),
            &[
                (2, Some("Core 0"), 48_000),
                (3, Some("Core 1"), 57_400),
                (4, Some("Core 2"), 52_000),
            ],
        );

        assert_eq!(
            values(&mut tree.sampler()),
            [(TemperatureKey::CpuPackage(1), Some(57))]
        );
    }

    #[test]
    fn via_cputemp_cores_form_one_package() {
        let tree = Tree::new("via");
        tree.hwmon(0, "via_cputemp", None, &[(1, Some("Core 0"), 40_000)]);
        tree.hwmon(1, "via_cputemp", None, &[(1, Some("Core 1"), 44_000)]);

        assert_eq!(
            values(&mut tree.sampler()),
            [(TemperatureKey::CpuPackage(0), Some(44))]
        );
    }

    #[test]
    fn k10temp_prefers_tdie_then_tctl_and_ignores_ccd_sensors() {
        let tree = Tree::new("k10temp");
        let node0 = tree.device("pci0000:00/0000:00:18.3");
        let node1 = tree.device("pci0000:00/0000:00:19.3");
        // Listed out of node order: hwmon numbering is not the package order.
        tree.hwmon(
            1,
            "k10temp",
            Some(&node1),
            &[(1, Some("Tctl"), 71_000), (3, Some("Tccd1"), 90_000)],
        );
        tree.hwmon(
            2,
            "k10temp",
            Some(&node0),
            &[
                (1, Some("Tctl"), 70_000),
                (2, Some("Tdie"), 60_000),
                (3, Some("Tccd1"), 95_000),
            ],
        );

        let mut readings = values(&mut tree.sampler());
        readings.sort_by_key(|(key, _)| format!("{key:?}"));
        assert_eq!(
            readings,
            [
                (TemperatureKey::CpuPackage(0), Some(60)),
                (TemperatureKey::CpuPackage(1), Some(71)),
            ]
        );
    }

    #[test]
    fn zenpower_with_only_tctl_uses_it() {
        let tree = Tree::new("zenpower");
        let node = tree.device("pci0000:00/0000:00:18.3");
        tree.hwmon(
            0,
            "zenpower",
            Some(&node),
            &[(1, Some("Tctl"), 45_500), (3, Some("Tccd1"), 50_000)],
        );

        assert_eq!(
            values(&mut tree.sampler()),
            [(TemperatureKey::CpuPackage(0), Some(46))]
        );
    }

    #[test]
    fn amdgpu_prefers_the_edge_sensor_over_junction() {
        let tree = Tree::new("amdgpu");
        let gpu = tree.device("pci0000:00/0000:03:00.0");
        tree.gpu(0, &gpu, "amdgpu");
        tree.power(&gpu, "auto", "active");
        let hwmon = tree.hwmon(
            5,
            "amdgpu",
            Some(&gpu),
            &[
                (1, Some("junction"), 70_000),
                (2, Some("edge"), 55_000),
                (3, Some("mem"), 60_000),
            ],
        );
        tree.write(&hwmon.join("temp2_crit"), "100000");

        let mut sampler = tree.sampler();
        let temperatures = sampler.sample(Instant::now()).to_vec();
        assert_eq!(temperatures.len(), 1);
        assert_eq!(temperatures[0].key, device_key(&gpu));
        assert_eq!(temperatures[0].celsius, Some(55));
        assert_eq!(temperatures[0].crit, Some(100));
    }

    #[test]
    fn nouveau_uses_its_first_sensor() {
        let tree = Tree::new("nouveau");
        let gpu = tree.device("pci0000:00/0000:01:00.0");
        tree.gpu(1, &gpu, "nouveau");
        tree.hwmon(0, "nouveau", Some(&gpu), &[(1, None, 42_000)]);

        assert_eq!(values(&mut tree.sampler()), [(device_key(&gpu), Some(42))]);
    }

    #[test]
    fn intel_igpu_without_its_own_sensor_shows_nothing() {
        let tree = Tree::new("i915");
        let gpu = tree.device("pci0000:00/0000:00:02.0");
        tree.gpu(0, &gpu, "i915");
        // i915 registers energy/power hwmon attributes without temperatures.
        let hwmon = tree.hwmon(0, "i915", Some(&gpu), &[]);
        tree.write(&hwmon.join("energy1_input"), "123");

        assert!(values(&mut tree.sampler()).is_empty());
    }

    #[test]
    fn nvme_matches_by_path_and_falls_back_to_the_controller_name() {
        let tree = Tree::new("nvme");
        // Path match: hwmon on the controller, as is the disk's device.
        let controller0 = tree.device("pci0000:00/0000:02:00.0/nvme/nvme0");
        tree.disk("nvme0n1", &controller0);
        tree.hwmon(
            0,
            "nvme",
            Some(&controller0),
            &[
                (1, Some("Composite"), 38_850),
                (2, Some("Sensor 1"), 50_000),
            ],
        );
        // Name fallback: hwmon on the PCI function, disk on a multipath subsystem.
        let pci = tree.device("pci0000:00/0000:05:00.0");
        tree.device("pci0000:00/0000:05:00.0/nvme/nvme1");
        let subsystem = tree.device("virtual/nvme-subsystem/nvme-subsys1");
        tree.disk("nvme1n1", &subsystem);
        tree.disk("nvme1n2", &subsystem);
        tree.hwmon(1, "nvme", Some(&pci), &[(1, Some("Composite"), 45_000)]);

        assert_eq!(
            values(&mut tree.sampler()),
            [
                (device_key(&controller0), Some(39)),
                (device_key(&subsystem), Some(45))
            ]
        );
    }

    #[test]
    fn nvme_hwmon_named_after_its_controller_matches_its_namespaces() {
        let tree = Tree::new("nvme-name");
        let controller = tree.device("pci0000:00/0000:02:00.0/nvme/nvme3");
        let namespace = tree.device("pci0000:00/0000:02:00.0/nvme/nvme3/ns1");
        tree.disk("nvme3n1", &namespace);
        tree.hwmon(
            0,
            "nvme",
            Some(&controller),
            &[(1, Some("Composite"), 40_000)],
        );

        assert_eq!(
            values(&mut tree.sampler()),
            [(device_key(&namespace), Some(40))]
        );
    }

    #[test]
    fn drivetemp_matches_its_disk_and_other_disks_get_nothing() {
        let tree = Tree::new("drivetemp");
        let sda = tree.device("pci0000:00/ata1/host0/target0:0:0/0:0:0:0");
        let sdb = tree.device("pci0000:00/ata2/host1/target1:0:0/1:0:0:0");
        tree.disk("sda", &sda);
        tree.disk("sdb", &sdb);
        tree.hwmon(0, "drivetemp", Some(&sda), &[(1, None, 33_000)]);

        assert_eq!(values(&mut tree.sampler()), [(device_key(&sda), Some(33))]);
    }

    #[test]
    fn nic_sensor_on_a_child_device_belongs_to_the_nic() {
        let tree = Tree::new("nic");
        let nic = tree.device("pci0000:00/0000:06:00.0");
        let phy = tree.device("pci0000:00/0000:06:00.0/mdio_bus/r8169-0-600/r8169-0-600:00");
        tree.nic("enp6s0", &nic);
        let hwmon = tree.hwmon(3, "r8169_0_600:00", Some(&phy), &[(1, None, 47_000)]);
        tree.write(&hwmon.join("temp1_max"), "120000");
        // An unrelated hwmon (RAM SPD) matches nothing.
        let spd = tree.device("pci0000:00/0000:00:14.0/i2c-6/6-0051");
        tree.hwmon(4, "spd5118", Some(&spd), &[(1, None, 40_000)]);

        let mut sampler = tree.sampler();
        let temperatures = sampler.sample(Instant::now()).to_vec();
        assert_eq!(temperatures.len(), 1);
        assert_eq!(temperatures[0].key, device_key(&nic));
        assert_eq!(
            (temperatures[0].celsius, temperatures[0].max),
            (Some(47), Some(120))
        );
    }

    #[test]
    fn hwmon_without_a_device_link_matches_no_device() {
        let tree = Tree::new("no-device");
        let disk = tree.device("pci0000:00/0000:02:00.0/nvme/nvme0");
        tree.disk("nvme0n1", &disk);
        tree.hwmon(0, "nvme", None, &[(1, Some("Composite"), 40_000)]);
        tree.hwmon(1, "acpitz", None, &[(1, None, 27_800)]);

        assert!(values(&mut tree.sampler()).is_empty());
    }

    #[test]
    fn thermal_zones_stand_in_for_a_cpu_without_hwmon_but_never_acpitz() {
        let tree = Tree::new("thermal");
        tree.zone(0, "acpitz", 90_000);
        tree.zone(1, "cpu-thermal", 51_000);
        tree.zone(2, "soc_thermal", 53_000);
        tree.zone(3, "gpu-thermal", 70_000);

        assert_eq!(
            values(&mut tree.sampler()),
            [(TemperatureKey::CpuPackage(0), Some(53))]
        );

        let acpi_only = Tree::new("thermal-acpi");
        acpi_only.zone(0, "acpitz", 40_000);
        assert!(values(&mut acpi_only.sampler()).is_empty());

        let x86 = Tree::new("thermal-x86");
        x86.zone(0, "x86_pkg_temp", 44_000);
        assert_eq!(
            values(&mut x86.sampler()),
            [(TemperatureKey::CpuPackage(0), Some(44))]
        );
    }

    #[test]
    fn thermal_zones_are_ignored_when_a_cpu_hwmon_exists() {
        let tree = Tree::new("thermal-hwmon");
        tree.zone(0, "x86_pkg_temp", 90_000);
        tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 50_000)]);

        assert_eq!(
            values(&mut tree.sampler()),
            [(TemperatureKey::CpuPackage(0), Some(50))]
        );
    }

    #[test]
    fn readings_convert_rounded_and_reject_implausible_values() {
        assert_eq!(celsius_from_millidegrees(38_499), Some(38));
        assert_eq!(celsius_from_millidegrees(38_500), Some(39));
        assert_eq!(celsius_from_millidegrees(-12_500), Some(-13));
        assert_eq!(celsius_from_millidegrees(-40_000), Some(-40));
        assert_eq!(celsius_from_millidegrees(150_000), Some(150));
        assert_eq!(celsius_from_millidegrees(-41_000), None);
        assert_eq!(celsius_from_millidegrees(151_000), None);
        assert_eq!(celsius_from_millidegrees(i64::MAX), None);
        assert_eq!(celsius_from_millidegrees(i64::MIN), None);

        let tree = Tree::new("invalid");
        let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 255_000)]);
        let mut sampler = tree.sampler();
        assert_eq!(
            values(&mut sampler),
            [(TemperatureKey::CpuPackage(0), None)]
        );
        tree.write(&hwmon.join("temp1_input"), "garbage");
        let later = Instant::now() + READ_INTERVAL;
        assert_eq!(sampler.sample(later)[0].celsius, None);
        assert!(
            !sampler.rediscover,
            "an invalid value is not a read failure"
        );
    }

    #[test]
    fn nvidia_rtd3_policy_keeps_nvml_open_only_when_the_gpu_cannot_suspend() {
        let status =
            |value: &str| format!("Runtime D3 status:          {value}\nVideo Memory:  Active\n");
        for (control, proc_power, keeps_open) in [
            (Some("on"), None, true),
            (Some("on"), Some(status("Enabled (fine-grained)")), true),
            (Some("auto"), Some(status("Not supported")), true),
            (Some("auto"), Some(status("Disabled by default")), true),
            (Some("auto"), Some(status("Disabled")), true),
            (Some("auto"), Some(status("Enabled (fine-grained)")), false),
            (
                Some("auto"),
                Some(status("Enabled (coarse-grained)")),
                false,
            ),
            (Some("auto"), Some(status("Something new")), false),
            (Some("auto"), Some("Video Memory: Active\n".into()), false),
            (Some("auto"), None, false),
            (None, None, false),
        ] {
            assert_eq!(
                nvidia_keeps_open(control, proc_power.as_deref()),
                keeps_open,
                "{control:?} {proc_power:?}"
            );
        }
    }

    #[test]
    fn nvidia_rtd3_policy_is_read_from_the_driver_at_discovery() {
        let tree = Tree::new("nvidia-policy");
        let gpu = tree.device("pci0000:00/0000:01:00.0");
        tree.gpu(1, &gpu, "nvidia");
        tree.power(&gpu, "auto", "active");
        let policy = |tree: &Tree| {
            discover(&tree.roots(), true)
                .sensors
                .into_iter()
                .find_map(|sensor| match sensor.source {
                    Source::Nvidia {
                        keep_open, bus_id, ..
                    } => Some((bus_id, keep_open)),
                    _ => None,
                })
        };

        assert_eq!(
            policy(&tree),
            Some(("0000:01:00.0".into(), false)),
            "missing file"
        );
        tree.write(
            &tree.0.join("proc/nvidia/gpus/0000:01:00.0/power"),
            "Runtime D3 status:          Not supported\n",
        );
        assert_eq!(policy(&tree), Some(("0000:01:00.0".into(), true)));
    }

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    /// Sample times at `interval` (ms) with alternating `jitter` (ms), and
    /// which of those samples read the sensors.
    fn reads(interval: u64, jitter: i64, samples: usize) -> Vec<bool> {
        let tree = Tree::new("cadence");
        let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 40_000)]);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
        (0..samples)
            .map(|index| {
                let offset = if index % 2 == 0 { jitter } else { -jitter };
                let millis = (interval * index as u64).saturating_add_signed(if index == 0 {
                    0
                } else {
                    offset
                });
                // A new value each sample shows whether this sample read it.
                let value = 40_000 + 1_000 * index as i64;
                tree.write(&hwmon.join("temp1_input"), &value.to_string());
                sampler.sample(at(start, millis))[0].celsius == Some(40 + index as i16)
            })
            .collect()
    }

    #[test]
    fn temperatures_are_read_at_most_every_two_seconds_whatever_the_interval() {
        let every =
            |n: usize, count: usize| (0..count).map(|index| index % n == 0).collect::<Vec<_>>();
        assert_eq!(reads(250, 0, 17), every(8, 17));
        assert_eq!(reads(1_000, 0, 7), every(2, 7));
        assert_eq!(reads(60_000, 0, 3), every(1, 3));
    }

    #[test]
    fn timer_jitter_at_a_two_second_interval_does_not_skip_reads() {
        assert!(reads(2_000, 50, 8).into_iter().all(|read| read));
        assert!(reads(2_000, -50, 8).into_iter().all(|read| read));
    }

    #[test]
    fn values_carry_over_between_reads_and_follow_interval_changes() {
        let tree = Tree::new("carry");
        let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 40_000)]);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
        let mut sample = |millis: u64, value: i64| {
            tree.write(&hwmon.join("temp1_input"), &value.to_string());
            sampler.sample(at(start, millis))[0].celsius
        };

        assert_eq!(sample(0, 40_000), Some(40));
        assert_eq!(sample(250, 41_000), Some(40), "carried over");
        assert_eq!(sample(500, 42_000), Some(40), "carried over");
        // The interval switches from 250 ms to 60 s: every sample reads.
        assert_eq!(sample(60_500, 43_000), Some(43));
        assert_eq!(sample(120_500, 44_000), Some(44));
        // And back to 1 s.
        assert_eq!(sample(121_500, 45_000), Some(44));
        assert_eq!(sample(122_500, 46_000), Some(46));
    }

    #[test]
    fn a_failing_sensor_triggers_rate_limited_rediscovery() {
        let tree = Tree::new("rediscover");
        let device = tree.device("pci0000:00/0000:02:00.0/nvme/nvme0");
        tree.disk("nvme0n1", &device);
        let hwmon = tree.hwmon(0, "nvme", Some(&device), &[(1, Some("Composite"), 40_000)]);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
        assert_eq!(sampler.sample(start)[0].celsius, Some(40));

        // The driver re-registers its hwmon under a new number.
        fs::remove_dir_all(&hwmon).unwrap();
        tree.hwmon(7, "nvme", Some(&device), &[(1, Some("Composite"), 41_000)]);
        assert_eq!(sampler.sample(at(start, 2_000))[0].celsius, None);
        assert!(sampler.rediscover);
        // Not before 30 s have passed since the last discovery.
        assert_eq!(sampler.sample(at(start, 28_000))[0].celsius, None);
        assert_eq!(sampler.sample(at(start, 30_000))[0].celsius, Some(41));
        assert!(!sampler.rediscover);
    }

    #[test]
    fn a_sensor_that_never_worked_does_not_trigger_rediscovery() {
        let tree = Tree::new("never-worked");
        let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 40_000)]);
        fs::remove_file(hwmon.join("temp1_input")).unwrap();
        fs::create_dir(hwmon.join("temp1_input")).unwrap(); // listed, but unreadable
        let mut sampler = tree.sampler();

        assert_eq!(
            values(&mut sampler),
            [(TemperatureKey::CpuPackage(0), None)]
        );
        assert!(!sampler.rediscover);
    }

    #[test]
    fn a_suspended_gpu_is_not_read_and_does_not_trigger_rediscovery() {
        let tree = Tree::new("suspended");
        let gpu = tree.device("pci0000:00/0000:03:00.0");
        tree.gpu(0, &gpu, "amdgpu");
        tree.power(&gpu, "auto", "active");
        let hwmon = tree.hwmon(0, "amdgpu", Some(&gpu), &[(1, Some("edge"), 50_000)]);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
        assert_eq!(sampler.sample(start)[0].celsius, Some(50));

        // Suspended: amdgpu refuses reads (EPERM); here the file is gone.
        tree.write(&gpu.join("power/runtime_status"), "suspended");
        fs::remove_file(hwmon.join("temp1_input")).unwrap();
        assert_eq!(sampler.sample(at(start, 2_000))[0].celsius, None);
        assert!(!sampler.rediscover);

        tree.write(&gpu.join("power/runtime_status"), "active");
        tree.write(&hwmon.join("temp1_input"), "52000");
        assert_eq!(sampler.sample(at(start, 4_000))[0].celsius, Some(52));
    }

    /// Bus ids asked for and the `keep_open` flag, per call.
    type NvidiaCalls = Rc<RefCell<Vec<(Vec<String>, bool)>>>;

    /// Records what the sampler asks of NVML.
    #[derive(Clone, Default)]
    struct FakeNvidia {
        calls: NvidiaCalls,
        releases: Rc<RefCell<usize>>,
        reading: Rc<RefCell<NvidiaReading>>,
        /// Whether each call asked for a full reading.
        full_reads: Rc<RefCell<Vec<bool>>>,
    }

    impl FakeNvidia {
        fn reporting(celsius: i64) -> Self {
            let fake = Self::default();
            fake.reading.borrow_mut().temperature = Some(celsius);
            fake
        }
    }

    impl NvidiaSource for FakeNvidia {
        fn read(
            &mut self,
            bus_ids: &[&str],
            full: bool,
            keep_open: bool,
            _now: Instant,
        ) -> Vec<NvidiaReading> {
            self.full_reads.borrow_mut().push(full);
            self.calls.borrow_mut().push((
                bus_ids.iter().map(|id| (*id).to_owned()).collect(),
                keep_open,
            ));
            vec![*self.reading.borrow(); bus_ids.len()]
        }

        fn release(&mut self) {
            *self.releases.borrow_mut() += 1;
        }
    }

    fn nvidia_tree(label: &str, control: &str, status: &str) -> (Tree, PathBuf) {
        let tree = Tree::new(label);
        let gpu = tree.device("pci0000:00/0000:01:00.0");
        tree.gpu(1, &gpu, "nvidia");
        tree.power(&gpu, control, status);
        (tree, gpu)
    }

    #[test]
    fn nvml_is_never_used_without_an_nvidia_gpu() {
        let tree = Tree::new("no-nvidia");
        let gpu = tree.device("pci0000:00/0000:03:00.0");
        tree.gpu(0, &gpu, "amdgpu");
        tree.hwmon(0, "amdgpu", Some(&gpu), &[(1, Some("edge"), 50_000)]);
        let fake = FakeNvidia::default();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), Instant::now());

        sampler.sample(Instant::now());
        assert!(fake.calls.borrow().is_empty());
        assert_eq!(*fake.releases.borrow(), 0);
    }

    #[test]
    fn nvidia_gpus_get_no_entry_while_nvidia_temperatures_are_off() {
        let (tree, _) = nvidia_tree("nvidia-off", "on", "active");
        let mut sampler: TemperatureSampler<FakeNvidia> =
            TemperatureSampler::new(tree.roots(), None, Instant::now());

        assert!(values(&mut sampler).is_empty());
        assert!(discover(&tree.roots(), false).sensors.is_empty());
    }

    #[test]
    fn nvidia_gpu_that_cannot_suspend_keeps_nvml_open() {
        let (tree, gpu) = nvidia_tree("nvidia-on", "on", "active");
        let fake = FakeNvidia::reporting(52);
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), Instant::now());

        assert_eq!(values(&mut sampler), [(device_key(&gpu), Some(52))]);
        assert_eq!(
            *fake.calls.borrow(),
            [(vec!["0000:01:00.0".to_owned()], true)]
        );
    }

    #[test]
    fn nvidia_gpu_with_rtd3_is_read_per_cycle_and_never_woken() {
        let (tree, gpu) = nvidia_tree("nvidia-rtd3", "auto", "active");
        let fake = FakeNvidia::reporting(48);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);

        assert_eq!(sampler.sample(start)[0].celsius, Some(48));
        assert_eq!(
            *fake.calls.borrow(),
            [(vec!["0000:01:00.0".to_owned()], false)]
        );

        tree.write(&gpu.join("power/runtime_status"), "suspended");
        assert_eq!(
            sampler.sample(at(start, 2_000))[0].celsius,
            None,
            "shown as –"
        );
        assert_eq!(
            fake.calls.borrow().len(),
            1,
            "NVML not called while suspended"
        );
        assert_eq!(*fake.releases.borrow(), 1);
        assert!(!sampler.rediscover);
    }

    #[test]
    fn nvml_values_are_validated_and_unavailable_nvml_shows_a_known_sensor() {
        let (tree, gpu) = nvidia_tree("nvidia-values", "on", "active");
        let mut unavailable = TemperatureSampler::new(tree.roots(), Some(NoNvidia), Instant::now());
        assert_eq!(values(&mut unavailable), [(device_key(&gpu), None)]);

        let implausible = FakeNvidia::reporting(4_000);
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(implausible), Instant::now());
        assert_eq!(values(&mut sampler), [(device_key(&gpu), None)]);
    }

    #[test]
    fn the_limit_is_the_critical_temperature_else_the_maximum() {
        let temperature = |max, crit| Temperature {
            key: TemperatureKey::CpuPackage(0),
            celsius: Some(50),
            max,
            crit,
        };
        assert_eq!(temperature(Some(80), Some(90)).limit(), Some(90));
        assert_eq!(temperature(Some(80), None).limit(), Some(80));
        assert_eq!(temperature(None, None).limit(), None);
    }

    #[test]
    fn open_nvml_refreshes_telemetry_every_sample_and_temperatures_every_two_seconds() {
        let (tree, gpu) = nvidia_tree("nvidia-telemetry", "on", "active");
        let fake = FakeNvidia::default();
        *fake.reading.borrow_mut() = NvidiaReading {
            temperature: Some(50),
            slowdown: Some(90),
            utilization: Some(30),
            memory: Some((4 << 30, 16 << 30)),
            power_milliwatts: Some(120_500),
            fan_percent: Some(40),
        };
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);

        let temperature = sampler.sample(start)[0].clone();
        assert_eq!(
            (temperature.celsius, temperature.max),
            (Some(50), Some(90)),
            "slowdown as max"
        );
        assert_eq!(
            sampler.gpus(),
            [GpuTelemetry {
                device_path: Arc::from(gpu.as_path()),
                utilization: Some(30.0),
                vram: Some(ByteUsage {
                    used: 4 << 30,
                    total: 16 << 30
                }),
                power_watts: Some(120.5),
                fan_percent: Some(40.0),
            }]
        );

        {
            let mut reading = fake.reading.borrow_mut();
            reading.temperature = Some(60);
            reading.utilization = Some(90);
            reading.fan_percent = Some(70);
        }
        assert_eq!(
            sampler.sample(at(start, 1_000))[0].celsius,
            Some(50),
            "temperature carried"
        );
        assert_eq!(
            sampler.gpus()[0].utilization,
            Some(90.0),
            "utilization refreshed"
        );
        assert_eq!(sampler.gpus()[0].fan_percent, Some(40.0), "fan carried");
        assert_eq!(sampler.sample(at(start, 2_000))[0].celsius, Some(60));
        assert_eq!(sampler.gpus()[0].fan_percent, Some(70.0));
        assert_eq!(fake.calls.borrow().len(), 3);
        // The costly queries (temperature, memory, fan) only every 2 s.
        assert_eq!(*fake.full_reads.borrow(), [true, false, true]);
    }

    #[test]
    fn nvml_initialized_per_reading_follows_the_temperature_cadence() {
        let (tree, _) = nvidia_tree("nvidia-rtd3-cadence", "auto", "active");
        let fake = FakeNvidia::reporting(45);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
        for millis in [0, 250, 500, 1_000, 1_500, 2_000] {
            sampler.sample(at(start, millis));
        }
        assert_eq!(fake.calls.borrow().len(), 2, "at 0 and 2 s only");
    }

    #[test]
    fn kernel_driver_gpus_report_telemetry_while_awake() {
        let tree = Tree::new("amdgpu-telemetry");
        let gpu = tree.device("pci0000:00/0000:03:00.0");
        tree.gpu(0, &gpu, "amdgpu");
        tree.power(&gpu, "auto", "active");
        tree.write(&gpu.join("gpu_busy_percent"), "37");
        let hwmon = tree.hwmon(0, "amdgpu", Some(&gpu), &[(1, Some("edge"), 50_000)]);
        tree.write(&hwmon.join("power1_average"), "80000000");
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);

        sampler.sample(start);
        assert_eq!(sampler.gpus()[0].utilization, Some(37.0));
        assert_eq!(sampler.gpus()[0].power_watts, Some(80.0));
        tree.write(&gpu.join("gpu_busy_percent"), "64");
        sampler.sample(at(start, 250));
        assert_eq!(sampler.gpus()[0].utilization, Some(64.0), "every sample");

        tree.write(&gpu.join("power/runtime_status"), "suspended");
        sampler.sample(at(start, 500));
        assert_eq!(
            sampler.gpus(),
            [GpuTelemetry::unavailable(Arc::from(gpu.as_path()))],
            "a suspended GPU is not read"
        );
    }

    #[test]
    fn nvidia_gpus_have_no_telemetry_while_nvidia_temperatures_are_off() {
        let (tree, _) = nvidia_tree("nvidia-off-telemetry", "on", "active");
        let mut sampler: TemperatureSampler<FakeNvidia> =
            TemperatureSampler::new(tree.roots(), None, Instant::now());
        sampler.sample(Instant::now());
        assert!(sampler.gpus().is_empty());
    }

    #[test]
    fn zenpower_reports_package_power_as_core_plus_soc() {
        let tree = Tree::new("zenpower-power");
        let node = tree.device("pci0000:00/0000:00:18.3");
        let hwmon = tree.hwmon(0, "zenpower", Some(&node), &[(1, Some("Tctl"), 45_000)]);
        tree.write(&hwmon.join("power1_input"), "38250000");
        tree.write(&hwmon.join("power2_input"), "11750000");
        let mut sampler = tree.sampler();
        sampler.sample(Instant::now());
        assert_eq!(sampler.cpu_power_watts(), Some(50.0));

        let k10temp = Tree::new("k10temp-power");
        let node = k10temp.device("pci0000:00/0000:00:18.3");
        k10temp.hwmon(0, "k10temp", Some(&node), &[(1, Some("Tctl"), 45_000)]);
        let mut sampler = k10temp.sampler();
        sampler.sample(Instant::now());
        assert_eq!(sampler.cpu_power_watts(), None, "k10temp has no power");
    }

    fn rapl_domain(tree: &Tree, domain: &str, name: &str, energy: u64) -> PathBuf {
        let path = tree.0.join("class/powercap").join(domain);
        tree.write(&path.join("name"), name);
        tree.write(&path.join("energy_uj"), &energy.to_string());
        tree.write(&path.join("max_energy_range_uj"), "262143328850");
        path.join("energy_uj")
    }

    #[test]
    fn readable_rapl_packages_give_power_from_energy_deltas() {
        let tree = Tree::new("rapl");
        let energy = rapl_domain(&tree, "intel-rapl:0", "package-0", 1_000_000);
        // A subdomain is part of the package; counting it would count twice.
        rapl_domain(&tree, "intel-rapl:0:0", "core", 500_000);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);

        sampler.sample(start);
        assert_eq!(
            sampler.cpu_power_watts(),
            None,
            "no delta on the first read"
        );
        tree.write(&energy, "51000000");
        sampler.sample(at(start, 2_000));
        assert_eq!(sampler.cpu_power_watts(), Some(25.0), "50 J in 2 s");

        // The counter wraps at max_energy_range_uj.
        tree.write(&energy, &(262_143_328_850_u64 - 1_000_000).to_string());
        sampler.sample(at(start, 4_000));
        tree.write(&energy, "9000000");
        sampler.sample(at(start, 6_000));
        assert_eq!(sampler.cpu_power_watts(), Some(5.0), "10 J across the wrap");
    }

    #[test]
    fn unreadable_rapl_counters_show_no_power_and_zenpower_comes_first() {
        let tree = Tree::new("rapl-unreadable");
        let path = tree.0.join("class/powercap/intel-rapl:0");
        tree.write(&path.join("name"), "package-0");
        // Standing in for a root-only file: listed, but it cannot be read.
        fs::create_dir_all(path.join("energy_uj")).unwrap();
        assert_eq!(
            discover(&tree.roots(), false).cpu_power,
            CpuPowerSource::None
        );

        let both = Tree::new("rapl-and-zenpower");
        rapl_domain(&both, "intel-rapl:0", "package-0", 1);
        let node = both.device("pci0000:00/0000:00:18.3");
        let hwmon = both.hwmon(0, "zenpower", Some(&node), &[(1, Some("Tctl"), 45_000)]);
        both.write(&hwmon.join("power1_input"), "20000000");
        assert!(matches!(
            discover(&both.roots(), false).cpu_power,
            CpuPowerSource::Hwmon(_)
        ));
    }

    #[test]
    fn an_inactive_sampler_reads_nothing_and_reads_at_once_when_active_again() {
        let (tree, gpu) = nvidia_tree("nvidia-inactive", "on", "active");
        let hwmon = tree.hwmon(0, "k10temp", None, &[(1, Some("Tctl"), 40_000)]);
        let fake = FakeNvidia::reporting(50);
        let start = Instant::now();
        let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
        let values = |sampler: &mut TemperatureSampler<FakeNvidia>, millis| {
            sampler
                .sample(at(start, millis))
                .iter()
                .map(|temperature| temperature.celsius)
                .collect::<Vec<_>>()
        };
        assert_eq!(values(&mut sampler, 0), [Some(40), Some(50)]);
        assert_eq!(fake.calls.borrow().len(), 1);

        sampler.set_active(false);
        tree.write(&hwmon.join("temp1_input"), "45000");
        *fake.reading.borrow_mut() = NvidiaReading {
            temperature: Some(55),
            ..NvidiaReading::default()
        };
        for millis in [1_000, 2_000, 10_000] {
            assert_eq!(values(&mut sampler, millis), [Some(40), Some(50)], "kept");
        }
        assert_eq!(
            fake.calls.borrow().len(),
            1,
            "NVML untouched while inactive"
        );
        assert_eq!(sampler.gpus()[0].device_path, Arc::from(gpu.as_path()));

        // Active again 100 ms after the last inactive call: read at once,
        // without waiting for the 2 s gate.
        sampler.set_active(true);
        assert_eq!(values(&mut sampler, 10_100), [Some(45), Some(55)]);
        assert_eq!(fake.calls.borrow().len(), 2);
    }
}
