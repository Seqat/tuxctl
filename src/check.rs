//! `tuxctl --check`: which sensors `tuxctl` finds on this machine, and what
//! would enable the missing ones. Printed before the terminal is touched.

use std::fmt::Write as _;

use crate::{
    about,
    linux::{
        CpuPowerAccess, GpuTelemetry, NvidiaAccess, SensorReport, StorageKind, Temperature,
        TemperatureKey,
    },
    text,
};

const SEE_README: &str = "see \"Optional setup\" in the README";

pub fn report_text(report: &SensorReport) -> String {
    let mut out = format!("{} {} sensor check\n", about::NAME, about::VERSION);
    let temperature = |key: &TemperatureKey| {
        report
            .temperatures
            .iter()
            .find(|(temperature, _)| temperature.key == *key)
    };

    for (index, cpu) in report.inventory.cpus.iter().enumerate() {
        section(&mut out, "CPU", &cpu.model);
        let package = cpu.physical_id.unwrap_or(index as u32);
        match temperature(&TemperatureKey::CpuPackage(package)) {
            Some((reading, origin)) => row(&mut out, "temperature", &celsius(reading), origin),
            None => row(&mut out, "temperature", "–", "no CPU sensor found"),
        }
        if index == 0 {
            let (value, note) = match report.cpu_power {
                CpuPowerAccess::Readable(origin) => ("readable", format!("from {origin}")),
                CpuPowerAccess::RootOnly => (
                    "–",
                    format!("RAPL energy counters are root-only; {SEE_README}"),
                ),
                CpuPowerAccess::Unavailable => ("–", "not reported on this machine".into()),
            };
            row(&mut out, "power", value, &note);
        }
    }

    for gpu in &report.inventory.gpus {
        let path = gpu.device_path.as_ref();
        let driver = path.and_then(|path| {
            report
                .gpu_drivers
                .iter()
                .find(|(known, _)| known == path)
                .map(|(_, driver)| driver.as_str())
        });
        let title = match driver {
            Some(driver) => format!("{}  ({driver})", gpu.model),
            None => gpu.model.clone(),
        };
        section(&mut out, "GPU", &title);
        let nvidia = driver == Some("nvidia");
        let nvml_note = match report.nvidia {
            NvidiaAccess::Off => Some("NVML is off (--no-nvidia-temperature)".to_owned()),
            NvidiaAccess::Unsupported => Some(format!(
                "this static build cannot load NVML; use a glibc build (cargo install); {SEE_README}"
            )),
            NvidiaAccess::On => None,
        };
        let reading = path.and_then(|path| temperature(&TemperatureKey::Device(path.clone())));
        match (reading, nvidia.then_some(nvml_note).flatten()) {
            (_, Some(note)) => row(&mut out, "temperature", "–", &note),
            (Some((reading, origin)), None) => {
                let origin = if reading.celsius.is_none() && nvidia {
                    "NVML: no value now (GPU asleep, or libnvidia-ml.so.1 missing)"
                } else {
                    origin
                };
                row(&mut out, "temperature", &celsius(reading), origin);
            }
            (None, None) => row(&mut out, "temperature", "–", "no sensor for this GPU"),
        }
        let telemetry =
            path.and_then(|path| report.gpus.iter().find(|gpu| gpu.device_path == *path));
        let details = telemetry.map(telemetry_text).unwrap_or_default();
        if nvidia && report.nvidia != NvidiaAccess::On {
            row(&mut out, "usage", "–", "needs NVML (see above)");
        } else if details.is_empty() {
            let note = format!("not reported by {}", driver.unwrap_or("its driver"));
            row(&mut out, "usage", "–", &note);
        } else {
            let origin = if nvidia { "NVML" } else { "sysfs" };
            row(&mut out, "usage", &details, origin);
        }
    }

    let mut counts = [0_usize; 5];
    for disk in &report.inventory.storage_devices {
        let (label, index) = storage_label(disk.kind);
        let number = counts[index];
        counts[index] += 1;
        let model = disk.model.as_deref().unwrap_or(&disk.system_name);
        section(
            &mut out,
            "Disk",
            &format!("{label}{number}  {model}  ({})", disk.system_name),
        );
        let reading = disk
            .device_path
            .as_ref()
            .and_then(|path| temperature(&TemperatureKey::Device(path.clone())));
        match reading {
            Some((reading, origin)) => row(&mut out, "temperature", &celsius(reading), origin),
            None if matches!(disk.kind, StorageKind::Sata | StorageKind::Scsi)
                && !report.drivetemp_loaded =>
            {
                row(
                    &mut out,
                    "temperature",
                    "–",
                    &format!("load drivetemp: sudo modprobe drivetemp; {SEE_README}"),
                );
            }
            None if matches!(disk.kind, StorageKind::Sata | StorageKind::Scsi) => row(
                &mut out,
                "temperature",
                "–",
                "drivetemp is loaded; the drive reports none",
            ),
            None => row(&mut out, "temperature", "–", "no sensor for this disk"),
        }
    }

    for nic in &report.inventory.network_devices {
        let title = match &nic.model {
            Some(model) => format!("{}  {model}", nic.interface_name),
            None => nic.interface_name.clone(),
        };
        section(&mut out, "Network", &title);
        let reading = nic
            .device_path
            .as_ref()
            .and_then(|path| temperature(&TemperatureKey::Device(path.clone())));
        match reading {
            Some((reading, origin)) => row(&mut out, "temperature", &celsius(reading), origin),
            None => row(&mut out, "temperature", "–", "no sensor for this adapter"),
        }
    }
    // Model names come from device firmware (USB product strings, NVMe
    // identify data); keep their escape sequences away from the terminal.
    text::strip_unsafe_lines(&out)
}

fn section(out: &mut String, kind: &str, title: &str) {
    let _ = write!(out, "\n{kind:<8}{title}\n");
}

fn row(out: &mut String, name: &str, value: &str, note: &str) {
    // A long value runs past its column; two spaces keep the note apart.
    let gap = if value.chars().count() >= 10 {
        "  "
    } else {
        " "
    };
    let _ = writeln!(out, "        {name:<12} {value:<10}{gap}{note}");
}

fn celsius(temperature: &Temperature) -> String {
    temperature
        .celsius
        .map_or_else(|| "–".into(), |celsius| format!("{celsius}°C"))
}

/// `util 3%, VRAM 1.8/15.9 GiB, 28 W, fan 0%`, from whatever is known.
fn telemetry_text(telemetry: &GpuTelemetry) -> String {
    const GIB: f64 = (1_u64 << 30) as f64;
    let mut parts = Vec::new();
    if let Some(utilization) = telemetry.utilization {
        parts.push(format!("util {utilization:.0}%"));
    }
    if let Some(vram) = telemetry.vram {
        parts.push(format!(
            "VRAM {:.1}/{:.1} GiB",
            vram.used as f64 / GIB,
            vram.total as f64 / GIB
        ));
    }
    if let Some(watts) = telemetry.power_watts {
        parts.push(format!("{watts:.0} W"));
    }
    if let Some(fan) = telemetry.fan_percent {
        parts.push(format!("fan {fan:.0}%"));
    }
    parts.join(", ")
}

/// The label and counter slot of a disk, as on the Overview.
fn storage_label(kind: StorageKind) -> (&'static str, usize) {
    match kind {
        StorageKind::Nvme => ("NVMe", 0),
        StorageKind::Sata => ("SATA", 1),
        StorageKind::Scsi => ("SCSI", 2),
        StorageKind::Virtio => ("VIRT", 3),
        StorageKind::Mmc => ("MMC", 4),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use super::*;
    use crate::linux::{ByteUsage, CpuPackage, GpuDevice, HardwareInventory, StorageDevice};

    fn path(name: &str) -> Arc<Path> {
        Arc::from(Path::new(name))
    }

    fn temperature(key: TemperatureKey, celsius: Option<i16>) -> Temperature {
        Temperature {
            key,
            celsius,
            max: None,
            crit: None,
        }
    }

    fn report(nvidia: NvidiaAccess, drivetemp_loaded: bool) -> SensorReport {
        SensorReport {
            inventory: HardwareInventory {
                cpus: vec![CpuPackage {
                    physical_id: Some(0),
                    model: "AMD Ryzen 5 7500F 6-Core Processor".into(),
                }],
                gpus: vec![GpuDevice {
                    model: "NVIDIA GeForce RTX 5070 Ti".into(),
                    kind: None,
                    vram_bytes: None,
                    device_path: Some(path("/gpu")),
                }],
                storage_devices: vec![
                    StorageDevice {
                        system_name: "nvme0n1".into(),
                        kind: StorageKind::Nvme,
                        model: Some("WD Blue".into()),
                        capacity_bytes: None,
                        device_path: Some(path("/nvme")),
                    },
                    StorageDevice {
                        system_name: "sda".into(),
                        kind: StorageKind::Sata,
                        model: Some("Samsung SSD 870".into()),
                        capacity_bytes: None,
                        device_path: Some(path("/sata")),
                    },
                ],
                ..HardwareInventory::default()
            },
            temperatures: vec![
                (
                    temperature(TemperatureKey::CpuPackage(0), Some(54)),
                    "k10temp Tctl".into(),
                ),
                (
                    temperature(TemperatureKey::Device(path("/nvme")), Some(41)),
                    "nvme Composite".into(),
                ),
                (
                    temperature(TemperatureKey::Device(path("/gpu")), Some(43)),
                    "NVML".into(),
                ),
            ],
            gpus: vec![GpuTelemetry {
                device_path: path("/gpu"),
                utilization: Some(3.0),
                vram: Some(ByteUsage {
                    used: 2 << 30,
                    total: 16 << 30,
                }),
                power_watts: Some(28.0),
                fan_percent: Some(0.0),
            }],
            gpu_drivers: vec![(path("/gpu"), "nvidia".into())],
            cpu_power: CpuPowerAccess::RootOnly,
            drivetemp_loaded,
            nvidia,
        }
    }

    fn line<'a>(text: &'a str, start: &str) -> &'a str {
        text.lines()
            .find(|line| line.trim_start().starts_with(start))
            .unwrap_or_else(|| panic!("{start}:\n{text}"))
    }

    #[test]
    fn firmware_strings_cannot_reach_the_terminal_as_escapes() {
        let mut report = report(NvidiaAccess::On, false);
        report.inventory.storage_devices[0].model = Some("WD\u{1b}]52;c;cm0=\u{7}Blue".into());
        report.inventory.gpus[0].model = "RTX\u{202E}evil\u{9b}2J".into();

        let text = report_text(&report);

        assert!(
            !text.chars().any(|c| c != '\n' && text::is_unsafe(c)),
            "{text:?}"
        );
        assert!(text.contains("WD]52;c;cm0=Blue"), "{text}");
        assert!(text.contains("RTXevil2J"), "{text}");
    }

    #[test]
    fn found_sensors_name_their_value_and_origin() {
        let text = report_text(&report(NvidiaAccess::On, false));
        assert!(text.starts_with("tuxctl "), "{text}");
        assert!(
            text.contains("CPU     AMD Ryzen 5 7500F 6-Core Processor"),
            "{text}"
        );
        assert!(
            text.contains("temperature  54°C       k10temp Tctl"),
            "{text}"
        );
        assert!(
            text.contains("temperature  41°C       nvme Composite"),
            "{text}"
        );
        assert!(
            text.contains("NVIDIA GeForce RTX 5070 Ti  (nvidia)"),
            "{text}"
        );
        assert!(
            text.contains("util 3%, VRAM 2.0/16.0 GiB, 28 W, fan 0%  NVML"),
            "{text}"
        );
    }

    #[test]
    fn missing_sensors_say_what_would_enable_them() {
        let text = report_text(&report(NvidiaAccess::On, false));
        assert!(
            line(&text, "power").contains("RAPL energy counters are root-only"),
            "{text}"
        );
        let sata = text.split("SATA0").nth(1).unwrap();
        assert!(sata.contains("sudo modprobe drivetemp"), "{text}");

        let loaded = report_text(&report(NvidiaAccess::On, true));
        assert!(
            loaded.contains("drivetemp is loaded; the drive reports none"),
            "{loaded}"
        );
    }

    #[test]
    fn nvidia_gpus_explain_why_nvml_is_not_used() {
        let off = report_text(&report(NvidiaAccess::Off, true));
        assert!(
            off.contains("NVML is off (--no-nvidia-temperature)"),
            "{off}"
        );
        assert!(off.contains("needs NVML (see above)"), "{off}");
        let unsupported = report_text(&report(NvidiaAccess::Unsupported, true));
        assert!(
            unsupported.contains("this static build cannot load NVML"),
            "{unsupported}"
        );
    }
}
