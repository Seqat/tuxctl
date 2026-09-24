//! GPU utilization, memory, power and fan. NVIDIA GPUs of the proprietary
//! driver are read through NVML by the temperature sampler; the others
//! (amdgpu, radeon, nouveau, i915, xe) through sysfs here. Values a driver
//! does not expose stay `None`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::system::ByteUsage;

/// Plausible ranges; anything else is a driver glitch, not a reading.
const MAX_WATTS: f64 = 2_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct GpuTelemetry {
    /// Canonical sysfs path of the GPU, as in the hardware inventory.
    pub device_path: Arc<Path>,
    /// Busy percentage of the graphics engine.
    pub utilization: Option<f64>,
    pub vram: Option<ByteUsage>,
    pub power_watts: Option<f64>,
    /// Fan speed as a percentage of its maximum.
    pub fan_percent: Option<f64>,
}

impl GpuTelemetry {
    pub(super) fn unavailable(device_path: Arc<Path>) -> Self {
        Self {
            device_path,
            utilization: None,
            vram: None,
            power_watts: None,
            fan_percent: None,
        }
    }
}

/// Reads a GPU driven by a kernel driver. `hwmon` is the GPU's own hwmon
/// directory, if it has one. The caller has checked that the GPU is awake.
pub(super) fn read_sysfs(device_path: &Arc<Path>, hwmon: Option<&Path>) -> GpuTelemetry {
    let device = &**device_path;
    let utilization = read_number(device.join("gpu_busy_percent"))
        .filter(|percent| (0.0..=100.0).contains(percent));
    let vram = match (
        read_number(device.join("mem_info_vram_used")),
        read_number(device.join("mem_info_vram_total")),
    ) {
        (Some(used), Some(total)) if total > 0.0 => Some(ByteUsage {
            used: used as u64,
            total: total as u64,
        }),
        _ => None,
    };
    let power_watts = hwmon.and_then(|hwmon| {
        // Microwatts; amdgpu offers an average, newer kernels an input.
        read_number(hwmon.join("power1_average"))
            .or_else(|| read_number(hwmon.join("power1_input")))
            .map(|microwatts| microwatts / 1_000_000.0)
            .filter(|watts| (0.0..=MAX_WATTS).contains(watts))
    });
    let fan_percent = hwmon.and_then(|hwmon| {
        let pwm = read_number(hwmon.join("pwm1"))?;
        let max = read_number(hwmon.join("pwm1_max")).unwrap_or(255.0);
        (max > 0.0 && (0.0..=max).contains(&pwm)).then(|| pwm / max * 100.0)
    });
    GpuTelemetry {
        device_path: Arc::clone(device_path),
        utilization,
        vram,
        power_watts,
        fan_percent,
    }
}

fn read_number(path: PathBuf) -> Option<f64> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "tuxctl-gpu-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("device/hwmon")).unwrap();
            Self(root)
        }

        fn device(&self) -> Arc<Path> {
            Arc::from(self.0.join("device").as_path())
        }

        fn hwmon(&self) -> PathBuf {
            self.0.join("device/hwmon")
        }

        fn write(&self, relative: &str, contents: &str) {
            fs::write(self.0.join(relative), contents).unwrap();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn amdgpu_reports_busy_vram_power_and_fan() {
        let tree = Tree::new("amdgpu");
        tree.write("device/gpu_busy_percent", "37\n");
        tree.write("device/mem_info_vram_used", "4294967296\n");
        tree.write("device/mem_info_vram_total", "17179869184\n");
        tree.write("device/hwmon/power1_average", "123000000\n");
        tree.write("device/hwmon/pwm1", "102\n");

        let telemetry = read_sysfs(&tree.device(), Some(&tree.hwmon()));
        assert_eq!(telemetry.utilization, Some(37.0));
        assert_eq!(
            telemetry.vram,
            Some(ByteUsage {
                used: 4 << 30,
                total: 16 << 30
            })
        );
        assert_eq!(telemetry.power_watts, Some(123.0));
        assert_eq!(telemetry.fan_percent, Some(40.0));
    }

    #[test]
    fn power_input_stands_in_for_a_missing_average_and_pwm_max_scales_the_fan() {
        let tree = Tree::new("nouveau");
        tree.write("device/hwmon/power1_input", "45500000\n");
        tree.write("device/hwmon/pwm1", "50\n");
        tree.write("device/hwmon/pwm1_max", "100\n");

        let telemetry = read_sysfs(&tree.device(), Some(&tree.hwmon()));
        assert_eq!(
            telemetry.utilization, None,
            "nouveau has no busy percentage"
        );
        assert_eq!(telemetry.vram, None);
        assert_eq!(telemetry.power_watts, Some(45.5));
        assert_eq!(telemetry.fan_percent, Some(50.0));
    }

    #[test]
    fn missing_and_implausible_values_stay_unknown() {
        let tree = Tree::new("i915");
        let nothing = read_sysfs(&tree.device(), None);
        assert_eq!(nothing, GpuTelemetry::unavailable(tree.device()));

        tree.write("device/gpu_busy_percent", "250\n");
        tree.write("device/mem_info_vram_used", "5\n");
        tree.write("device/mem_info_vram_total", "0\n");
        tree.write("device/hwmon/power1_average", "999999999999\n");
        tree.write("device/hwmon/pwm1", "300\n");
        let glitches = read_sysfs(&tree.device(), Some(&tree.hwmon()));
        assert_eq!(glitches, GpuTelemetry::unavailable(tree.device()));
    }
}
