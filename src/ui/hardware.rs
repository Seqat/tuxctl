use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::linux::{GpuKind, HardwareInventory, MemoryModule, StorageDevice, StorageKind};

pub fn render(
    frame: &mut Frame,
    inventory: Option<&HardwareInventory>,
    fallback_memory: Option<u64>,
    area: Rect,
) {
    let block = Block::default().borders(Borders::ALL).title(" Hardware ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = inventory.map_or_else(
        || vec!["Discovering hardware…".to_owned()],
        |inventory| inventory_lines(inventory, fallback_memory),
    );
    let lines = fit_lines(lines, usize::from(inner.height))
        .into_iter()
        .map(|line| {
            if matches!(line.as_str(), "CPU" | "RAM" | "GPU" | "STORAGE") {
                Line::from(format!("▸ {line}")).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(line)
            }
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn inventory_lines(inventory: &HardwareInventory, fallback_memory: Option<u64>) -> Vec<String> {
    let mut lines = vec!["CPU".into()];
    if inventory.cpus.is_empty() {
        lines.push("  Unavailable".into());
    } else {
        lines.extend(
            inventory
                .cpus
                .iter()
                .enumerate()
                .map(|(index, cpu)| format!("  CPU{index}: {}", cpu.model)),
        );
    }

    lines.push("RAM".into());
    lines.extend(
        inventory
            .memory_modules
            .iter()
            .enumerate()
            .map(|(index, module)| format_memory_module(index, module)),
    );
    let total_memory = inventory.total_memory.or(fallback_memory);
    lines.push(format!(
        "  TOTAL: {}",
        total_memory
            .map(format_binary_capacity)
            .unwrap_or_else(|| "N/A".into())
    ));

    lines.push("GPU".into());
    if inventory.gpus.is_empty() {
        lines.push("  Unavailable / none detected".into());
    } else {
        lines.extend(inventory.gpus.iter().enumerate().map(|(index, gpu)| {
            let kind = match gpu.kind {
                Some(GpuKind::Integrated) => " (iGPU)",
                Some(GpuKind::Discrete) => " (dGPU)",
                None => "",
            };
            let vram = gpu
                .vram_bytes
                .map(|bytes| format!(" - {} VRAM", format_binary_capacity(bytes)))
                .unwrap_or_default();
            format!("  GPU{index}: {}{kind}{vram}", gpu.model)
        }));
    }

    lines.push("STORAGE".into());
    if inventory.storage_devices.is_empty() {
        lines.push("  Unavailable / none detected".into());
    } else {
        let mut counts = [0_usize; 5];
        lines.extend(inventory.storage_devices.iter().map(|device| {
            let (label, count_index) = storage_label(device.kind);
            let index = counts[count_index];
            counts[count_index] += 1;
            format_storage_device(label, index, device)
        }));
    }

    lines
}

fn format_memory_module(index: usize, module: &MemoryModule) -> String {
    let mut details = vec![format_binary_capacity(module.capacity_bytes)];
    if let Some(memory_type) = &module.memory_type {
        if let Some(speed) = module.speed_mts {
            details.push(format!("{memory_type}-{speed}"));
        } else {
            details.push(memory_type.clone());
        }
    } else if let Some(speed) = module.speed_mts {
        details.push(format!("{speed} MT/s"));
    }
    details.extend(module.manufacturer.iter().cloned());
    details.extend(module.part_number.iter().cloned());
    if let Some(locator) = &module.locator {
        details.push(format!("({locator})"));
    }
    format!("  SLOT{index}: {}", details.join(" "))
}

fn storage_label(kind: StorageKind) -> (&'static str, usize) {
    match kind {
        StorageKind::Nvme => ("NVMe", 0),
        StorageKind::Sata => ("SATA", 1),
        StorageKind::Scsi => ("SCSI", 2),
        StorageKind::Virtio => ("VIRT", 3),
        StorageKind::Mmc => ("MMC", 4),
    }
}

fn format_storage_device(label: &str, index: usize, device: &StorageDevice) -> String {
    let model = device.model.as_deref().unwrap_or(&device.system_name);
    let capacity = device
        .capacity_bytes
        .map(|bytes| format!(" - {}", format_decimal_capacity(bytes)))
        .unwrap_or_default();
    format!("  {label}{index}: {model}{capacity}")
}

fn fit_lines(mut lines: Vec<String>, height: usize) -> Vec<String> {
    if lines.len() <= height {
        return lines;
    }
    if height == 0 {
        return Vec::new();
    }

    let hidden = lines.len().saturating_sub(height).saturating_add(1);
    lines.truncate(height.saturating_sub(1));
    lines.push(format!("… {hidden} more"));
    lines
}

fn format_binary_capacity(bytes: u64) -> String {
    format_capacity(bytes, 1024, ["B", "KiB", "MiB", "GiB", "TiB"])
}

fn format_decimal_capacity(bytes: u64) -> String {
    format_capacity(bytes, 1000, ["B", "KB", "MB", "GB", "TB"])
}

fn format_capacity(bytes: u64, base: u64, units: [&str; 5]) -> String {
    let mut divisor = 1_u64;
    let mut unit = 0;
    while bytes / divisor >= base && unit < units.len() - 1 {
        let Some(next) = divisor.checked_mul(base) else {
            break;
        };
        divisor = next;
        unit += 1;
    }

    if bytes.is_multiple_of(divisor) {
        format!("{} {}", bytes / divisor, units[unit])
    } else {
        format!("{:.1} {}", bytes as f64 / divisor as f64, units[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_binary_and_decimal_capacities() {
        assert_eq!(format_binary_capacity(16 * 1024 * 1024 * 1024), "16 GiB");
        assert_eq!(format_binary_capacity(512 * 1024 * 1024), "512 MiB");
        assert_eq!(format_decimal_capacity(1_000_000_000_000), "1 TB");
        assert_eq!(format_decimal_capacity(500_000_000_000), "500 GB");
    }

    #[test]
    fn missing_optional_module_metadata_stays_concise() {
        let module = MemoryModule {
            locator: None,
            capacity_bytes: 8 * 1024 * 1024 * 1024,
            memory_type: None,
            speed_mts: None,
            manufacturer: None,
            part_number: None,
        };

        assert_eq!(format_memory_module(0, &module), "  SLOT0: 8 GiB");
    }

    #[test]
    fn total_only_ram_fallback_is_rendered() {
        let inventory = HardwareInventory {
            total_memory: Some(32 * 1024 * 1024 * 1024),
            ..HardwareInventory::default()
        };
        let lines = inventory_lines(&inventory, None);

        assert!(lines.contains(&"  TOTAL: 32 GiB".to_owned()));
        assert!(!lines.iter().any(|line| line.contains("SLOT")));
    }

    #[test]
    fn long_inventory_is_clipped_with_overflow_count() {
        let lines = (0..10).map(|index| format!("line {index}")).collect();

        assert_eq!(fit_lines(lines, 3), vec!["line 0", "line 1", "… 8 more"]);
    }
}
