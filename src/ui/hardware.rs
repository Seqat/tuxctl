use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::App,
    linux::{GpuKind, HardwareInventory, MemoryModule, StorageDevice, StorageKind, SystemMetrics},
};

use super::{format_bytes, hardware_cpu, hardware_network_summary, layout};

const MAX_RAM_GAUGE_WIDTH: usize = 36;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let block = Block::default().borders(Borders::ALL).title(" Hardware ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let inventory = app.hardware();
    let metrics = app.system_metrics();
    let grid_row_budget = usize::from(inner.height.saturating_sub(10).max(1));
    let cpu_grid = hardware_cpu::grid_layout(metrics, usize::from(inner.width), grid_row_budget);

    let desired = [
        hardware_cpu::desired_height(metrics, cpu_grid),
        2_u16.saturating_add(
            inventory
                .map(|inventory| inventory.memory_modules.len().min(2) as u16)
                .unwrap_or(0),
        ),
        1_u16.saturating_add(
            inventory
                .map(|inventory| inventory.gpus.len().clamp(1, 3) as u16)
                .unwrap_or(1),
        ),
        1_u16.saturating_add(
            inventory
                .map(|inventory| {
                    u16::try_from(inventory.storage_devices.len().max(1)).unwrap_or(u16::MAX)
                })
                .unwrap_or(1),
        ),
        hardware_network_summary::desired_height(app, inventory),
    ];
    let (heights, spacing) = allocate_section_heights(inner.height, desired);
    let areas = vertical_areas(inner, heights, spacing);

    hardware_cpu::render(frame, app, inventory, metrics, cpu_grid, areas[0]);
    render_ram(frame, inventory, metrics, areas[1]);
    render_gpu(frame, inventory, areas[2]);
    render_storage(frame, inventory, areas[3]);
    hardware_network_summary::render(frame, app, inventory, areas[4]);
}

fn allocate_section_heights(total: u16, desired: [u16; 5]) -> ([u16; 5], u16) {
    let mut heights = [0; 5];
    let spacing = u16::from(total >= 21);
    let mut remaining = total.saturating_sub(spacing.saturating_mul(4));

    for height in &mut heights {
        if remaining == 0 {
            return (heights, 0);
        }
        *height = 1;
        remaining -= 1;
    }

    for section in 0..heights.len() {
        let addition = desired[section]
            .saturating_sub(heights[section])
            .min(remaining);
        heights[section] += addition;
        remaining -= addition;
    }
    (heights, spacing)
}

fn vertical_areas(area: Rect, heights: [u16; 5], spacing: u16) -> [Rect; 5] {
    let mut y = area.y;
    let mut index = 0;
    heights.map(|height| {
        let height = height.min(area.bottom().saturating_sub(y));
        let result = Rect::new(area.x, y, area.width, height);
        y = y.saturating_add(height);
        if index < heights.len() - 1 && height > 0 {
            y = y.saturating_add(spacing).min(area.bottom());
        }
        index += 1;
        result
    })
}

fn render_ram(
    frame: &mut Frame,
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![section_heading("RAM")];

    if lines.len() < usize::from(area.height) {
        let usage_line = metrics.memory.map_or_else(
            || "Used  N/A".into(),
            |memory| ram_usage_line(memory, width),
        );
        lines.push(Line::from(layout::truncate(&usage_line, width)));
    }

    if let Some(inventory) = inventory {
        let remaining = usize::from(area.height).saturating_sub(lines.len());
        lines.extend(
            inventory
                .memory_modules
                .iter()
                .enumerate()
                .take(remaining)
                .map(|(index, module)| {
                    Line::from(layout::truncate(
                        &format_memory_module(index, module),
                        width,
                    ))
                }),
        );
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn ram_usage_line(memory: crate::linux::ByteUsage, width: usize) -> String {
    let percent = memory.percent();
    let percent_text = format!("{percent:.0}%");
    let usage = format_usage_compact(memory.used, memory.total);
    let fixed_width = "Used  ".len() + percent_text.chars().count() + 4 + usage.chars().count();
    let gauge_width = width.saturating_sub(fixed_width).min(MAX_RAM_GAUGE_WIDTH);
    if gauge_width < 4 {
        return layout::truncate(&format!("Used  {percent_text}  {usage}"), width);
    }
    layout::truncate(
        &format!(
            "Used  {percent_text}  {}  {usage}",
            utilization_bar(Some(percent), gauge_width)
        ),
        width,
    )
}

fn render_gpu(frame: &mut Frame, inventory: Option<&HardwareInventory>, area: Rect) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![section_heading("GPU")];
    if lines.len() < usize::from(area.height) {
        match inventory {
            None => lines.push(Line::from("Discovering hardware…")),
            Some(inventory) if inventory.gpus.is_empty() => {
                lines.push(Line::from("Unavailable / none detected"));
            }
            Some(inventory) => {
                let remaining = usize::from(area.height).saturating_sub(lines.len());
                let show_overflow = inventory.gpus.len() > remaining && remaining > 1;
                let device_limit = remaining.saturating_sub(usize::from(show_overflow));
                lines.extend(inventory.gpus.iter().take(device_limit).map(|gpu| {
                    let kind = match gpu.kind {
                        Some(GpuKind::Integrated) => "  iGPU",
                        Some(GpuKind::Discrete) => "  dGPU",
                        None => "",
                    };
                    let vram = gpu
                        .vram_bytes
                        .map(|bytes| format!("  {} VRAM", format_binary_capacity(bytes)))
                        .unwrap_or_default();
                    Line::from(layout::truncate(
                        &format!("{}{}{}", gpu.model, kind, vram),
                        width,
                    ))
                }));
                if show_overflow {
                    lines.push(Line::from(format!(
                        "… {} more GPUs",
                        inventory.gpus.len() - device_limit
                    )));
                }
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_storage(frame: &mut Frame, inventory: Option<&HardwareInventory>, area: Rect) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![section_heading("STORAGE")];
    if lines.len() < usize::from(area.height) {
        match inventory {
            None => lines.push(Line::from("Discovering hardware…")),
            Some(inventory) if inventory.storage_devices.is_empty() => {
                lines.push(Line::from("Unavailable / none detected"));
            }
            Some(inventory) => {
                let remaining = usize::from(area.height).saturating_sub(lines.len());
                let show_overflow = inventory.storage_devices.len() > remaining && remaining > 1;
                let device_limit = remaining.saturating_sub(usize::from(show_overflow));
                let mut counts = [0_usize; 5];
                for device in inventory.storage_devices.iter().take(device_limit) {
                    let (label, count_index) = storage_label(device.kind);
                    let index = counts[count_index];
                    counts[count_index] += 1;
                    lines.push(Line::from(layout::truncate(
                        &format_storage_device(label, index, device),
                        width,
                    )));
                }
                if show_overflow {
                    lines.push(Line::from(format!(
                        "… {} more devices",
                        inventory.storage_devices.len() - device_limit
                    )));
                }
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

pub(super) fn section_heading(label: &'static str) -> Line<'static> {
    Line::from(label).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

pub(super) fn utilization_bar(percent: Option<f64>, width: usize) -> String {
    let filled = percent
        .map(|percent| ((percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize)
        .unwrap_or(0)
        .min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn format_usage_compact(used: u64, total: u64) -> String {
    format!("{} / {}", format_bytes(used), format_bytes(total))
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
    format!("SLOT{index}  {}", details.join(" "))
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
        .map(|bytes| format!("  {}", format_decimal_capacity(bytes)))
        .unwrap_or_default();
    format!("{label}{index}  {model}{capacity}")
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
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;

    #[test]
    fn ram_gauge_expands_but_remains_bounded() {
        let memory = crate::linux::ByteUsage {
            used: 8 * 1024 * 1024 * 1024,
            total: 32 * 1024 * 1024 * 1024,
        };
        let wide = ram_usage_line(memory, 100);
        let narrow = ram_usage_line(memory, 32);

        assert_eq!(wide.matches(['█', '░']).count(), MAX_RAM_GAUGE_WIDTH);
        let percent = wide.find("25%").unwrap();
        let gauge = wide.find(['█', '░']).unwrap();
        let values = wide.find("8.0 GiB / 32.0 GiB").unwrap();
        assert!(percent < gauge);
        assert!(gauge < values);
        assert!(wide.chars().count() <= 100);
        assert!(narrow.chars().count() <= 32);
        assert!(narrow.starts_with("Used  25%"));
    }

    #[test]
    fn ram_render_omits_the_redundant_total_row() {
        let mut app = App::default();
        app.update(crate::action::Action::SystemMetricsUpdated(SystemMetrics {
            memory: Some(crate::linux::ByteUsage {
                used: 8 * 1024 * 1024 * 1024,
                total: 32 * 1024 * 1024 * 1024,
            }),
            ..SystemMetrics::default()
        }));
        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, &app, frame.area()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(text.contains("RAM"));
        assert!(text.contains("Used"));
        assert!(!text.contains("Total"));
    }

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

        assert_eq!(format_memory_module(0, &module), "SLOT0  8 GiB");
    }

    #[test]
    fn long_storage_names_can_be_safely_truncated() {
        let device = StorageDevice {
            system_name: "nvme0n1".into(),
            model: Some("A very long storage model name that exceeds the panel".into()),
            capacity_bytes: Some(1_000_000_000_000),
            kind: StorageKind::Nvme,
        };
        let line = layout::truncate(&format_storage_device("NVMe", 0, &device), 24);

        assert_eq!(line.chars().count(), 24);
        assert!(line.ends_with('…'));
    }

    #[test]
    fn section_height_allocation_never_exceeds_the_available_area() {
        for height in 0..40 {
            let (allocated, spacing) = allocate_section_heights(height, [20, 4, 4, 8, 5]);
            let used = allocated.into_iter().sum::<u16>() + spacing * 4;
            assert!(used <= height);
            if height >= 21 {
                assert_eq!(spacing, 1);
            }
        }
    }
}
