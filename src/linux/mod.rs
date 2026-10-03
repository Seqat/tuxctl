//! Everything that reads the system, on background threads the main loop polls:
//! processes, network and services each have a collector that publishes its
//! latest snapshot; one system-metrics worker also samples temperatures, GPUs
//! and CPU power; the journal streams entries through a bounded channel; and
//! the hardware inventory is sent once. Nothing outside this module touches
//! `/proc`, `/sys`, `systemctl` or `journalctl`.

mod check;
mod control;
mod gpu;
mod hardware;
mod journal;
mod latest_snapshot;
mod localtime;
mod network;
// Static musl builds cannot dlopen the (glibc) NVIDIA library.
#[cfg(not(target_env = "musl"))]
mod nvml;
mod process;
mod rate;
mod service;
mod system;
mod temperature;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub use check::{sensor_report, CpuPowerAccess, NvidiaAccess, SensorReport};
pub use gpu::GpuTelemetry;
#[cfg(test)]
pub(crate) use hardware::{CpuPackage, NetworkDevice};
pub use hardware::{
    GpuDevice, GpuKind, HardwareCollector, HardwareInventory, MemoryModule, StorageDevice,
    StorageKind,
};
pub use journal::{priority_label, JournalBatch, JournalCollector, JournalEntry};
#[cfg(test)]
pub(crate) use localtime::LocalTime;
pub use network::{NetworkCollector, NetworkInterfaceInfo, NetworkSnapshot, OperState};
#[cfg(test)]
pub use process::verify_and_send_signal_at;
pub use process::{
    send_process_signal, ProcessCollector, ProcessIdentity, ProcessInfo, ProcessSignal,
    ProcessSignalError, ProcessSnapshot, ProcessSummary,
};
pub use service::{
    ServiceCollector, ServiceInfo, ServiceRefreshGeneration, ServiceSnapshot, SYSTEMCTL_TIMEOUT,
};
#[cfg(test)]
pub(crate) use system::DiskIo;
#[cfg(test)]
pub(crate) use system::LoadAverage;
#[cfg(test)]
pub(crate) use system::LogicalCpuId;
pub use system::{ByteUsage, LogicalCpuMetrics, MountUsage, SystemMetrics, SystemMetricsCollector};
pub use temperature::{Temperature, TemperatureKey};

/// Directories searched for system tools before `PATH`.
const SYSTEM_BIN_DIRS: [&str; 2] = ["/usr/bin", "/bin"];

/// A command for the system tool `name` (`systemctl`, `journalctl`). The
/// system directories come first so that a root `tuxctl` started with a user's
/// `PATH` (`su` without `-`) never runs a same-named program from a directory
/// that user can write; `PATH` is only the fallback for systems that keep
/// them elsewhere, such as NixOS.
fn system_command(name: &str) -> Command {
    Command::new(system_tool_path(name, &SYSTEM_BIN_DIRS))
}

fn system_tool_path(name: &str, dirs: &[impl AsRef<Path>]) -> PathBuf {
    dirs.iter()
        .map(|dir| dir.as_ref().join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_tools_come_from_the_first_directory_that_has_them() {
        let root = std::env::temp_dir().join(format!("tuxctl-bin-{}", std::process::id()));
        let (first, second) = (root.join("first"), root.join("second"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("systemctl"), "").unwrap();
        std::fs::create_dir_all(first.join("journalctl")).unwrap();
        std::fs::write(second.join("journalctl"), "").unwrap();
        let dirs = [&first, &second];

        assert_eq!(
            system_tool_path("systemctl", &dirs),
            second.join("systemctl")
        );
        // A directory with the tool's name is not the tool.
        assert_eq!(
            system_tool_path("journalctl", &dirs),
            second.join("journalctl")
        );
        assert_eq!(system_tool_path("missing", &dirs), PathBuf::from("missing"));

        std::fs::write(first.join("systemctl"), "").unwrap();
        assert_eq!(
            system_tool_path("systemctl", &dirs),
            first.join("systemctl")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
