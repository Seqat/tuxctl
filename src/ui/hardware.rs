use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::App,
    linux::{
        GpuKind, HardwareInventory, LogicalCpuMetrics, MemoryModule, OverviewMetrics,
        StorageDevice, StorageKind,
    },
};

use super::{format_bytes, layout};

const DETAILED_CPU_CELL_WIDTH: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuGridLayout {
    columns: usize,
    rows: usize,
    visible: usize,
    dense: bool,
}

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
    let metrics = app.overview();
    let cpu_count = metrics.logical_cpus.len();
    let max_cpu_id = metrics
        .logical_cpus
        .iter()
        .map(|cpu| cpu.id.index())
        .max()
        .unwrap_or(0);
    let grid_row_budget = usize::from(inner.height.saturating_sub(8).max(1));
    let grid = cpu_grid_layout(
        cpu_count,
        max_cpu_id,
        usize::from(inner.width),
        grid_row_budget,
    );

    let desired = [
        4_u16
            .saturating_add(u16::try_from(grid.rows).unwrap_or(u16::MAX))
            .saturating_add(u16::from(grid.visible < cpu_count)),
        3_u16.saturating_add(
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
    ];
    let heights = allocate_section_heights(inner.height, desired);
    let areas = vertical_areas(inner, heights);

    render_cpu(frame, app, inventory, metrics, grid, areas[0]);
    render_ram(frame, inventory, metrics, areas[1]);
    render_gpu(frame, inventory, areas[2]);
    render_storage(frame, inventory, areas[3]);
}

fn allocate_section_heights(total: u16, desired: [u16; 4]) -> [u16; 4] {
    let mut heights = [0; 4];
    let mut remaining = total;

    for height in &mut heights {
        if remaining == 0 {
            return heights;
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
    heights[3] = heights[3].saturating_add(remaining);
    heights
}

fn vertical_areas(area: Rect, heights: [u16; 4]) -> [Rect; 4] {
    let mut y = area.y;
    heights.map(|height| {
        let height = height.min(area.bottom().saturating_sub(y));
        let result = Rect::new(area.x, y, area.width, height);
        y = y.saturating_add(height);
        result
    })
}

fn render_cpu(
    frame: &mut Frame,
    app: &App,
    inventory: Option<&HardwareInventory>,
    metrics: &OverviewMetrics,
    grid: CpuGridLayout,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![section_heading("CPU")];

    if lines.len() < usize::from(area.height) {
        let model = inventory
            .and_then(|inventory| inventory.cpus.first())
            .map(|cpu| cpu.model.as_str())
            .unwrap_or("Discovering hardware…");
        lines.push(Line::from(layout::truncate(model, width)));
    }
    if lines.len() < usize::from(area.height) {
        let percent = format_percent(metrics.cpu_percent);
        let prefix = format!("Util  {percent:>4}  ");
        let history_width = width.saturating_sub(prefix.chars().count());
        let history = history_sparkline(app.aggregate_cpu_history().iter(), history_width);
        lines.push(Line::from(layout::truncate(
            &format!("{prefix}{history}"),
            width,
        )));
    }
    if lines.len() < usize::from(area.height) {
        let load = metrics.load_average.map_or_else(
            || "Load  1m N/A  5m N/A  15m N/A".into(),
            |load| {
                format!(
                    "Load  1m {:.2}  5m {:.2}  15m {:.2}",
                    load.one, load.five, load.fifteen
                )
            },
        );
        lines.push(Line::from(layout::truncate(&load, width)));
    }

    let remaining = usize::from(area.height).saturating_sub(lines.len());
    lines.extend(
        cpu_grid_lines(&metrics.logical_cpus, width, grid)
            .into_iter()
            .take(remaining)
            .map(Line::from),
    );
    if grid.visible < metrics.logical_cpus.len() && lines.len() < usize::from(area.height) {
        lines.push(Line::from(format!(
            "… {} more logical CPUs",
            metrics.logical_cpus.len() - grid.visible
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn cpu_grid_layout(count: usize, max_cpu_id: u32, width: usize, max_rows: usize) -> CpuGridLayout {
    if count == 0 || width == 0 || max_rows == 0 {
        return CpuGridLayout {
            columns: 0,
            rows: 0,
            visible: 0,
            dense: false,
        };
    }

    let needed_columns = count.div_ceil(max_rows);
    let detailed_columns = (width / DETAILED_CPU_CELL_WIDTH).max(1).min(count);
    let dense_cell_width = format!("CPU{max_cpu_id}█100%").chars().count().max(8);
    let dense_columns = (width / dense_cell_width).max(1).min(count);
    let dense = needed_columns > detailed_columns;
    let columns = if dense {
        dense_columns.max(detailed_columns)
    } else {
        detailed_columns
    };
    let visible = count.min(columns.saturating_mul(max_rows));

    CpuGridLayout {
        columns,
        rows: visible.div_ceil(columns),
        visible,
        dense,
    }
}

fn cpu_grid_lines(cpus: &[LogicalCpuMetrics], width: usize, grid: CpuGridLayout) -> Vec<String> {
    if grid.columns == 0 || grid.visible == 0 {
        return Vec::new();
    }

    let cell_width = width / grid.columns;
    cpus[..grid.visible]
        .chunks(grid.columns)
        .map(|row| {
            row.iter()
                .map(|cpu| {
                    let text = if grid.dense {
                        format!(
                            "CPU{}{}{}",
                            cpu.id.index(),
                            utilization_level(cpu.utilization_percent),
                            format_percent(cpu.utilization_percent)
                        )
                    } else {
                        format!(
                            "CPU{:<3} {} {:>4}",
                            cpu.id.index(),
                            utilization_bar(cpu.utilization_percent, 5),
                            format_percent(cpu.utilization_percent)
                        )
                    };
                    pad_cell(&layout::truncate(&text, cell_width), cell_width)
                })
                .collect::<String>()
        })
        .collect()
}

fn render_ram(
    frame: &mut Frame,
    inventory: Option<&HardwareInventory>,
    metrics: &OverviewMetrics,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let total = inventory
        .and_then(|inventory| inventory.total_memory)
        .or_else(|| metrics.memory.map(|memory| memory.total));
    let mut lines = vec![section_heading("RAM")];

    if lines.len() < usize::from(area.height) {
        let total_line = format!(
            "Total  {}",
            total
                .map(format_binary_capacity)
                .unwrap_or_else(|| "N/A".into())
        );
        let usage_line = metrics.memory.map_or_else(
            || "Used  N/A".into(),
            |memory| {
                let percent = memory.percent();
                format!(
                    "Used  {}  {}",
                    utilization_bar(Some(percent), 8),
                    format_usage_compact(memory.used, memory.total, percent)
                )
            },
        );
        if area.height >= 3 {
            lines.push(Line::from(layout::truncate(&total_line, width)));
        }
        if lines.len() < usize::from(area.height) {
            lines.push(Line::from(layout::truncate(&usage_line, width)));
        }
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

fn section_heading(label: &'static str) -> Line<'static> {
    Line::from(label).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn history_sparkline(samples: impl ExactSizeIterator<Item = f64>, width: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if width == 0 {
        return String::new();
    }
    let skip = samples.len().saturating_sub(width);
    let result = samples
        .skip(skip)
        .map(|sample| {
            let index = ((sample.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize;
            LEVELS[index.min(LEVELS.len() - 1)]
        })
        .collect::<String>();
    if result.is_empty() {
        "—".into()
    } else {
        result
    }
}

fn utilization_level(percent: Option<f64>) -> char {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    percent.map_or('·', |percent| {
        let index = ((percent.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize;
        LEVELS[index.min(LEVELS.len() - 1)]
    })
}

fn utilization_bar(percent: Option<f64>, width: usize) -> String {
    let filled = percent
        .map(|percent| ((percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize)
        .unwrap_or(0)
        .min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn format_percent(percent: Option<f64>) -> String {
    percent
        .map(|percent| format!("{:.0}%", percent.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "N/A".into())
}

fn format_usage_compact(used: u64, total: u64, percent: f64) -> String {
    format!(
        "{} / {}  {percent:.0}%",
        format_bytes(used),
        format_bytes(total)
    )
}

fn pad_cell(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(padding))
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
    use super::*;

    #[test]
    fn one_logical_cpu_uses_one_detailed_cell() {
        assert_eq!(
            cpu_grid_layout(1, 0, 80, 6),
            CpuGridLayout {
                columns: 1,
                rows: 1,
                visible: 1,
                dense: false,
            }
        );
    }

    #[test]
    fn twelve_logical_cpus_form_a_balanced_detailed_grid() {
        let grid = cpu_grid_layout(12, 11, 80, 6);
        assert_eq!(grid.columns, 4);
        assert_eq!(grid.rows, 3);
        assert_eq!(grid.visible, 12);
        assert!(!grid.dense);
    }

    #[test]
    fn thirty_two_logical_cpus_switch_to_dense_cells() {
        let grid = cpu_grid_layout(32, 31, 48, 10);
        assert!(grid.dense);
        assert_eq!(grid.columns, 4);
        assert_eq!(grid.rows, 8);
        assert_eq!(grid.visible, 32);
    }

    #[test]
    fn high_logical_cpu_count_uses_width_and_height_budget() {
        let grid = cpu_grid_layout(128, 127, 100, 16);
        assert!(grid.dense);
        assert_eq!(grid.visible, 128);
        assert!(grid.rows <= 16);
    }

    #[test]
    fn narrow_and_tiny_cpu_grids_remain_bounded() {
        let narrow = cpu_grid_layout(12, 11, 9, 4);
        assert_eq!(narrow.columns, 1);
        assert_eq!(narrow.rows, 4);
        assert_eq!(narrow.visible, 4);

        assert_eq!(cpu_grid_layout(32, 31, 0, 10).visible, 0);
        assert_eq!(cpu_grid_layout(32, 31, 10, 0).visible, 0);
    }

    #[test]
    fn history_sparkline_keeps_the_newest_samples_that_fit() {
        let samples = [0.0, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 100.0];

        assert_eq!(history_sparkline(samples.into_iter(), 4), "▅▆▇█");
        assert_eq!(history_sparkline(samples.into_iter(), 0), "");
        assert_eq!(history_sparkline([].into_iter(), 4), "—");
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
    fn section_height_allocation_never_exceeds_the_available_area() {
        for height in 0..40 {
            let allocated = allocate_section_heights(height, [20, 5, 4, 8]);
            assert_eq!(allocated.into_iter().sum::<u16>(), height);
        }
    }
}
