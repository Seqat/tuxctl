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

use super::{format_bytes, hardware_cpu, hardware_network_summary, layout, network};

const MAX_RAM_GAUGE_WIDTH: usize = 36;
const MAX_STORAGE_ROWS: u16 = 4;
/// A section heading plus one value row; anything less is not drawn.
const LOWER_SECTION_MIN_HEIGHT: u16 = 2;

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
    let width = usize::from(inner.width);
    let lower_desired = [
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
                    inventory
                        .storage_devices
                        .len()
                        .clamp(1, MAX_STORAGE_ROWS.into()) as u16
                })
                .unwrap_or(1),
        ),
        hardware_network_summary::desired_height(app, inventory),
    ];
    let (heights, spacing) = allocate_section_heights(
        inner.height,
        hardware_cpu::priority_height(metrics, width),
        hardware_cpu::comfortable_height(metrics, width),
        lower_desired,
        |height| hardware_cpu::fitted_height(metrics, width, height),
    );
    let areas = vertical_areas(inner, heights, spacing);

    hardware_cpu::render(frame, app, inventory, metrics, areas[0]);
    render_ram(frame, inventory, metrics, areas[1]);
    render_gpu(frame, inventory, areas[2]);
    render_storage(frame, inventory, metrics, areas[3]);
    hardware_network_summary::render(frame, app, inventory, areas[4]);
}

/// Splits the panel between CPU and the lower sections (RAM, GPU, storage,
/// network), returning heights and the gap between sections.
///
/// 1. CPU gets its priority height (summary plus a few grid rows).
/// 2. Lower sections, in order, get a heading plus one value row; the first
///    that does not fit and every later one are omitted instead of being
///    drawn as an orphan heading.
/// 3. Lower sections grow toward their desired height.
/// 4. Only once every lower section is complete does CPU grow toward its
///    comfortable height, trimmed by `cpu_fitted` to the rows its grid
///    actually draws; leftover rows then become gaps between sections.
///
/// CPU only gains rows in step 1 and step 4, and neither can shrink as the
/// panel grows, so a taller panel never shows fewer CPUs.
fn allocate_section_heights(
    total: u16,
    cpu_priority: u16,
    cpu_comfortable: u16,
    lower_desired: [u16; 4],
    cpu_fitted: impl Fn(u16) -> u16,
) -> ([u16; 5], u16) {
    let mut heights = [0; 5];
    heights[0] = cpu_priority.min(total);
    let mut remaining = total - heights[0];

    let mut all_lower_shown = true;
    for height in &mut heights[1..] {
        if remaining < LOWER_SECTION_MIN_HEIGHT {
            all_lower_shown = false;
            break;
        }
        *height = LOWER_SECTION_MIN_HEIGHT;
        remaining -= LOWER_SECTION_MIN_HEIGHT;
    }

    let mut lower_complete = all_lower_shown;
    if all_lower_shown {
        for (height, desired) in heights[1..].iter_mut().zip(lower_desired) {
            let addition = desired.saturating_sub(*height).min(remaining);
            *height += addition;
            remaining -= addition;
            lower_complete &= *height >= desired;
        }
    }

    if lower_complete {
        let addition = cpu_comfortable.saturating_sub(heights[0]).min(remaining);
        let fitted = cpu_fitted(heights[0] + addition).max(heights[0]);
        remaining -= fitted - heights[0];
        heights[0] = fitted;
    }

    let gaps = heights
        .iter()
        .filter(|height| **height > 0)
        .count()
        .saturating_sub(1) as u16;
    let spacing = u16::from(gaps > 0 && remaining >= gaps);
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
            |memory| usage_bar_line("Used  ", memory, width),
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

/// Formats `<label>N%  [bar]  used / total`, dropping the bar when it would be too narrow.
pub(super) fn usage_bar_line(label: &str, usage: crate::linux::ByteUsage, width: usize) -> String {
    let percent = usage.percent();
    let percent_text = format!("{percent:.0}%");
    let usage = format_usage_compact(usage.used, usage.total);
    let fixed_width =
        label.chars().count() + percent_text.chars().count() + 4 + usage.chars().count();
    let gauge_width = width.saturating_sub(fixed_width).min(MAX_RAM_GAUGE_WIDTH);
    if gauge_width < 4 {
        return layout::truncate(&format!("{label}{percent_text}  {usage}"), width);
    }
    layout::truncate(
        &format!(
            "{label}{percent_text}  {}  {usage}",
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

fn render_storage(
    frame: &mut Frame,
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
    area: Rect,
) {
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
                    lines.push(Line::from(storage_line(
                        &format_storage_device(label, index, device),
                        disk_rates(metrics, &device.system_name),
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

/// Read/write rates of a storage device. `None` hides the rates entirely
/// (no disk statistics at all); a device missing from them shows `--`.
fn disk_rates(metrics: &SystemMetrics, name: &str) -> Option<(Option<f64>, Option<f64>)> {
    if metrics.disks.is_empty() {
        return None;
    }
    Some(
        metrics
            .disks
            .iter()
            .find(|disk| *disk.name == *name)
            .map_or((None, None), |disk| {
                (disk.read_bytes_per_sec, disk.write_bytes_per_sec)
            }),
    )
}

/// The device description with its rates right-aligned: the model is
/// truncated first, then the rates switch to a tight form, and they are left
/// out when not even the device label would remain.
fn storage_line(
    description: &str,
    rates: Option<(Option<f64>, Option<f64>)>,
    width: usize,
) -> String {
    let Some((read, write)) = rates else {
        return layout::truncate(description, width);
    };
    let label_width = description
        .split("  ")
        .next()
        .unwrap_or_default()
        .chars()
        .count();
    let candidates = [
        format!(
            "  R {}  W {}",
            network::format_rate(read),
            network::format_rate(write)
        ),
        format!(
            "  R{} W{}",
            hardware_network_summary::format_rate_tight(read),
            hardware_network_summary::format_rate_tight(write)
        ),
    ];
    candidates
        .iter()
        .find_map(|rates| {
            let room = width.checked_sub(rates.chars().count())?;
            (room >= label_width).then(|| {
                let description = layout::truncate(description, room);
                let padding = room.saturating_sub(description.chars().count());
                format!("{description}{}{rates}", " ".repeat(padding))
            })
        })
        .unwrap_or_else(|| layout::truncate(description, width))
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
        let wide = usage_bar_line("Used  ", memory, 100);
        let narrow = usage_bar_line("Used  ", memory, 32);

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
    fn storage_rates_are_right_aligned_and_the_model_truncates_first() {
        let description = "NVMe0  Samsung SSD 980 PRO 1TB  1.0 TB";
        let rates = Some((Some(12.3 * 1024.0 * 1024.0), Some(0.0)));

        let wide = storage_line(description, rates, 70);
        assert_eq!(wide.chars().count(), 70);
        assert!(wide.starts_with(description), "{wide}");
        assert!(wide.ends_with("  R 12.3 MiB/s  W 0 B/s"), "{wide}");

        let medium = storage_line(description, rates, 40);
        assert_eq!(medium.chars().count(), 40);
        assert!(medium.starts_with("NVMe0  Sams"), "{medium}");
        assert!(medium.ends_with("  R 12.3 MiB/s  W 0 B/s"), "{medium}");

        let narrow = storage_line(description, rates, 20);
        assert!(narrow.chars().count() <= 20, "{narrow}");
        assert!(narrow.starts_with("NVMe0"), "{narrow}");
        assert!(narrow.ends_with("  R12M/s W0B/s"), "{narrow}");

        let tiny = storage_line(description, rates, 8);
        assert_eq!(tiny, layout::truncate(description, 8), "rates dropped");
    }

    #[test]
    fn storage_rates_show_dashes_for_a_missing_disk_and_hide_without_statistics() {
        let mut metrics = SystemMetrics {
            disks: vec![crate::linux::DiskIo {
                name: "sda".into(),
                read_bytes_per_sec: Some(1024.0),
                write_bytes_per_sec: None,
            }],
            ..SystemMetrics::default()
        };

        assert_eq!(disk_rates(&metrics, "sda"), Some((Some(1024.0), None)));
        assert_eq!(disk_rates(&metrics, "nvme0n1"), Some((None, None)));
        assert!(
            storage_line("SATA0  disk", disk_rates(&metrics, "nvme0n1"), 40)
                .ends_with("  R --  W --")
        );

        metrics.disks.clear();
        assert_eq!(disk_rates(&metrics, "sda"), None);
        assert_eq!(storage_line("SATA0  disk", None, 40), "SATA0  disk");
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
        for height in 0..60 {
            let (allocated, spacing) = allocate_section_heights(height, 9, 20, [4, 4, 5, 5], |h| h);
            let gaps = allocated
                .iter()
                .filter(|height| **height > 0)
                .count()
                .saturating_sub(1);
            let used = allocated.into_iter().sum::<u16>() + spacing * gaps as u16;
            assert!(used <= height, "height {height}: {allocated:?} + {spacing}");
        }
    }

    #[test]
    fn lower_sections_get_a_value_row_or_are_omitted() {
        for height in 0..60 {
            let (allocated, _) = allocate_section_heights(height, 9, 20, [4, 4, 5, 5], |h| h);
            let lower = &allocated[1..];
            assert!(lower.iter().all(|height| *height == 0 || *height >= 2));
            let shown = lower.iter().take_while(|height| **height > 0).count();
            assert!(
                lower[shown..].iter().all(|height| *height == 0),
                "{allocated:?}"
            );
        }
    }

    #[test]
    fn unused_cpu_rows_become_section_gaps() {
        let (allocated, spacing) = allocate_section_heights(40, 9, 25, [4, 4, 5, 5], |h| h.min(18));
        assert_eq!(allocated, [18, 4, 4, 5, 5]);
        assert_eq!(spacing, 1);
    }

    #[test]
    fn cpu_priority_comes_first_and_extra_rows_wait_for_lower_sections() {
        assert_eq!(
            allocate_section_heights(6, 9, 20, [4, 4, 5, 5], |h| h).0,
            [6, 0, 0, 0, 0]
        );
        assert_eq!(
            allocate_section_heights(17, 9, 20, [4, 4, 5, 5], |h| h).0,
            [9, 2, 2, 2, 2]
        );
        assert_eq!(
            allocate_section_heights(27, 9, 20, [4, 4, 5, 5], |h| h).0,
            [9, 4, 4, 5, 5]
        );
        assert_eq!(
            allocate_section_heights(30, 9, 20, [4, 4, 5, 5], |h| h),
            ([12, 4, 4, 5, 5], 0)
        );
        assert_eq!(
            allocate_section_heights(44, 9, 20, [4, 4, 5, 5], |h| h),
            ([20, 4, 4, 5, 5], 1)
        );
    }
}
