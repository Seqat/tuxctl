//! What `tuxctl --check` reports: the sensors found on this machine, read
//! once, and what stands in the way of the missing ones.

use std::{path::PathBuf, time::Instant};

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

impl NvidiaAccess {
    /// What this build does with NVML, given `--no-nvidia-temperature`.
    pub fn new(nvidia_temperature: bool) -> Self {
        if !crate::cli::NVML_AVAILABLE {
            Self::Unsupported
        } else if nvidia_temperature {
            Self::On
        } else {
            Self::Off
        }
    }
}

pub struct SensorReport {
    pub inventory: HardwareInventory,
    /// Each temperature with where it comes from (`k10temp Tctl`).
    pub temperatures: Vec<(Temperature, String)>,
    pub gpus: Vec<GpuTelemetry>,
    pub cpu_power: CpuPowerAccess,
    pub drivetemp_loaded: bool,
    pub nvidia: NvidiaAccess,
}

/// Discovers the hardware and reads every sensor once.
pub fn sensor_report(nvidia_temperature: bool) -> SensorReport {
    let inventory = hardware::discover();
    let roots = SysfsRoots::default();
    let nvidia = NvidiaAccess::new(nvidia_temperature);
    let nvidia_on = nvidia == NvidiaAccess::On;
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
    SensorReport {
        gpus: sampler.gpus().to_vec(),
        inventory,
        temperatures,
        cpu_power,
        drivetemp_loaded: PathBuf::from("/sys/module/drivetemp").exists(),
        nvidia,
    }
}
