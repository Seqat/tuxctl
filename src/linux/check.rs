//! What `tuxctl --check` reports: the sensors found on this machine, read
//! once, and what stands in the way of the missing ones.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use super::{
    gpu::GpuTelemetry,
    hardware::{self, HardwareInventory},
    system::Nvidia,
    temperature::{self, SysfsRoots, Temperature, TemperatureSampler},
};

/// Whether CPU package power can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuPowerAccess {
    /// From this driver or interface.
    Readable(&'static str),
    /// RAPL counters exist but only root can read them.
    RootOnly,
    /// Nothing on this machine reports it.
    Unavailable,
}

/// Whether NVIDIA GPUs of the proprietary driver are read through NVML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvidiaAccess {
    On,
    /// `--no-nvidia-temperature`.
    Off,
    /// A static (musl) build, which cannot load NVML.
    Unsupported,
}

pub struct SensorReport {
    pub inventory: HardwareInventory,
    /// Each temperature with where it comes from (`k10temp Tctl`).
    pub temperatures: Vec<(Temperature, String)>,
    pub gpus: Vec<GpuTelemetry>,
    /// The kernel driver of each GPU, by device path.
    pub gpu_drivers: Vec<(Arc<Path>, String)>,
    pub cpu_power: CpuPowerAccess,
    pub drivetemp_loaded: bool,
    pub nvidia: NvidiaAccess,
}

/// Discovers the hardware and reads every sensor once.
pub fn sensor_report(nvidia_temperature: bool) -> SensorReport {
    let inventory = hardware::discover();
    let roots = SysfsRoots::default();
    let nvidia_on = nvidia_temperature && !cfg!(target_env = "musl");
    let now = Instant::now();
    let mut sampler = TemperatureSampler::new(roots.clone(), nvidia_on.then(Nvidia::default), now);
    let readings = sampler.sample(now).to_vec();
    let temperatures = readings
        .into_iter()
        .map(|temperature| {
            let origin = sampler
                .origin(&temperature.key)
                .unwrap_or_default()
                .to_owned();
            (temperature, origin)
        })
        .collect();
    let cpu_power = match sampler.cpu_power_origin() {
        Some(origin) => CpuPowerAccess::Readable(origin),
        None if temperature::rapl_present(&roots.powercap) => CpuPowerAccess::RootOnly,
        None => CpuPowerAccess::Unavailable,
    };
    let gpu_drivers = inventory
        .gpus
        .iter()
        .filter_map(|gpu| {
            let path = gpu.device_path.clone()?;
            let driver = fs::canonicalize(path.join("driver")).ok()?;
            Some((path, driver.file_name()?.to_str()?.to_owned()))
        })
        .collect();
    SensorReport {
        gpus: sampler.gpus().to_vec(),
        inventory,
        temperatures,
        gpu_drivers,
        cpu_power,
        drivetemp_loaded: PathBuf::from("/sys/module/drivetemp").exists(),
        nvidia: if cfg!(target_env = "musl") {
            NvidiaAccess::Unsupported
        } else if nvidia_temperature {
            NvidiaAccess::On
        } else {
            NvidiaAccess::Off
        },
    }
}
