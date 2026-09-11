use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::App,
    linux::{
        GpuKind, HardwareInventory, LogicalCpuMetrics, MemoryModule, NetworkInterfaceInfo,
        OperState, StorageDevice, StorageKind, SystemMetrics,
    },
};

use super::{format_bytes, layout};

const PREFERRED_CPU_CELL_WIDTH: usize = 20;
const MIN_DETAILED_CPU_CELL_WIDTH: usize = 16;
const MAX_DETAILED_CPU_COLUMNS: usize = 4;
const MIN_USEFUL_CPU_GAUGE_WIDTH: usize = 6;
const CPU_CELL_GAP: usize = 2;
const MAX_RAM_GAUGE_WIDTH: usize = 36;
const MAX_NETWORK_INTERFACES: usize = 3;

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
    let metrics = app.system_metrics();
    let cpu_count = metrics.logical_cpus.len();
    let max_cpu_id = metrics
        .logical_cpus
        .iter()
        .map(|cpu| cpu.id.index())
        .max()
        .unwrap_or(0);
    let grid_row_budget = usize::from(inner.height.saturating_sub(10).max(1));
    let grid = cpu_grid_layout(
        cpu_count,
        max_cpu_id,
        usize::from(inner.width),
        grid_row_budget,
    );

    let network_count = overview_network_interfaces(app.networks(), inventory).len();
    let desired = [
        6_u16
            .saturating_add(u16::try_from(grid.rows).unwrap_or(u16::MAX))
            .saturating_add(u16::from(grid.visible < cpu_count)),
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
        1_u16
            .saturating_add(u16::try_from(network_count.min(MAX_NETWORK_INTERFACES)).unwrap_or(3))
            .saturating_add(u16::from(network_count > MAX_NETWORK_INTERFACES)),
    ];
    let (heights, spacing) = allocate_section_heights(inner.height, desired);
    let areas = vertical_areas(inner, heights, spacing);

    render_cpu(frame, app, inventory, metrics, grid, areas[0]);
    render_ram(frame, inventory, metrics, areas[1]);
    render_gpu(frame, inventory, areas[2]);
    render_storage(frame, inventory, areas[3]);
    render_network(frame, app, inventory, areas[4]);
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

fn render_cpu(
    frame: &mut Frame,
    app: &App,
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
    grid: CpuGridLayout,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let grid_lines = cpu_grid_lines(&metrics.logical_cpus, width, grid);
    let has_overflow = grid.visible < metrics.logical_cpus.len();
    let content_height = 4_usize
        .saturating_add(grid_lines.len())
        .saturating_add(usize::from(has_overflow));
    let spacing = height.saturating_sub(content_height).min(2);
    let mut lines = vec![section_heading("CPU")];

    if lines.len() < height {
        let model = inventory
            .and_then(|inventory| inventory.cpus.first())
            .map(|cpu| cpu.model.as_str())
            .unwrap_or("Discovering hardware…");
        lines.push(Line::from(layout::truncate(model, width)));
    }
    if spacing >= 1 && lines.len() < height {
        lines.push(Line::from(""));
    }
    if lines.len() < height {
        let percent = format_percent(metrics.cpu_percent);
        let prefix = format!("Util  {percent:>4}  ");
        let history_width = width.saturating_sub(prefix.chars().count());
        let history = history_sparkline(app.aggregate_cpu_history().iter(), history_width);
        lines.push(Line::from(layout::truncate(
            &format!("{prefix}{history}"),
            width,
        )));
    }
    if lines.len() < height {
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
    if spacing >= 2 && lines.len() < height {
        lines.push(Line::from(""));
    }

    let remaining = height.saturating_sub(lines.len());
    lines.extend(grid_lines.into_iter().take(remaining).map(Line::from));
    if has_overflow && lines.len() < height {
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
    let preferred_columns = cpu_columns_that_fit(width, PREFERRED_CPU_CELL_WIDTH)
        .clamp(1, MAX_DETAILED_CPU_COLUMNS)
        .min(count);
    let detailed_capacity = cpu_columns_that_fit(width, MIN_DETAILED_CPU_CELL_WIDTH)
        .clamp(1, MAX_DETAILED_CPU_COLUMNS)
        .min(count);
    let dense_cell_width = format!("CPU{max_cpu_id}█100%").chars().count().max(8);
    let dense_columns = cpu_columns_that_fit(width, dense_cell_width)
        .max(1)
        .min(count);
    let dense = needed_columns > detailed_capacity;
    let columns = if dense {
        dense_columns.max(detailed_capacity)
    } else {
        preferred_columns.max(needed_columns.min(detailed_capacity))
    };
    let visible = count.min(columns.saturating_mul(max_rows));

    CpuGridLayout {
        columns,
        rows: visible.div_ceil(columns),
        visible,
        dense,
    }
}

fn cpu_columns_that_fit(width: usize, cell_width: usize) -> usize {
    width.saturating_add(CPU_CELL_GAP) / cell_width.saturating_add(CPU_CELL_GAP)
}

fn cpu_grid_lines(cpus: &[LogicalCpuMetrics], width: usize, grid: CpuGridLayout) -> Vec<String> {
    if grid.columns == 0 || grid.visible == 0 {
        return Vec::new();
    }

    let total_gap = CPU_CELL_GAP.saturating_mul(grid.columns.saturating_sub(1));
    let cell_width = width.saturating_sub(total_gap) / grid.columns;
    let gap = " ".repeat(CPU_CELL_GAP);
    let label_width = cpus[..grid.visible]
        .iter()
        .map(|cpu| format!("CPU{}", cpu.id.index()).chars().count())
        .max()
        .unwrap_or(3);
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
                        detailed_cpu_cell(cpu, cell_width, label_width)
                    };
                    pad_cell(&layout::truncate(&text, cell_width), cell_width)
                })
                .collect::<Vec<_>>()
                .join(&gap)
        })
        .collect()
}

fn detailed_cpu_cell(cpu: &LogicalCpuMetrics, cell_width: usize, label_width: usize) -> String {
    let label = format!("CPU{}", cpu.id.index());
    let percent = format_percent(cpu.utilization_percent);
    let fixed_width = label_width + 6;
    let gauge_width = cell_width.saturating_sub(fixed_width);
    if gauge_width < MIN_USEFUL_CPU_GAUGE_WIDTH {
        return layout::truncate(&format!("{label:<label_width$} {percent:>4}"), cell_width);
    }
    format!(
        "{label:<label_width$} {percent:>4} {}",
        utilization_bar(cpu.utilization_percent, gauge_width)
    )
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

fn render_network(frame: &mut Frame, app: &App, inventory: Option<&HardwareInventory>, area: Rect) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![section_heading("NETWORK")];
    let remaining = usize::from(area.height).saturating_sub(1);
    if remaining == 0 {
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    if app.networks().is_empty() {
        lines.push(Line::from(if app.network_error().is_some() {
            "Network data unavailable"
        } else {
            "No interfaces found"
        }));
    } else {
        let interfaces = overview_network_interfaces(app.networks(), inventory);
        if interfaces.is_empty() {
            lines.push(Line::from("No hardware interfaces found"));
            frame.render_widget(Paragraph::new(lines), area);
            return;
        }
        let show_overflow = interfaces.len() > MAX_NETWORK_INTERFACES && remaining > 1;
        let interface_limit = interfaces
            .len()
            .min(MAX_NETWORK_INTERFACES)
            .min(remaining.saturating_sub(usize::from(show_overflow)));
        lines.extend(interfaces.iter().take(interface_limit).map(|interface| {
            let model = inventory.and_then(|inventory| {
                inventory
                    .network_devices
                    .iter()
                    .find(|device| device.interface_name == interface.name)
                    .and_then(|device| device.model.as_deref())
            });
            network_summary_line(interface, model, width)
        }));
        if show_overflow {
            lines.push(Line::from(format!(
                "… {} more interfaces",
                interfaces.len() - interface_limit
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn overview_network_interfaces<'a>(
    interfaces: &'a [NetworkInterfaceInfo],
    inventory: Option<&HardwareInventory>,
) -> Vec<&'a NetworkInterfaceInfo> {
    let is_physical = |name: &str| {
        inventory.is_some_and(|inventory| {
            inventory
                .network_devices
                .iter()
                .any(|device| device.interface_name == name)
        })
    };
    let mut relevant = interfaces
        .iter()
        .filter(|interface| is_physical(&interface.name) || !is_noisy_network_name(&interface.name))
        .collect::<Vec<_>>();
    relevant.sort_by_key(|interface| !is_physical(&interface.name));
    relevant
}

fn is_noisy_network_name(name: &str) -> bool {
    name == "lo"
        || [
            "docker",
            "veth",
            "br-",
            "virbr",
            "cni",
            "flannel",
            "cali",
            "kube",
            "vboxnet",
            "vmnet",
            "tun",
            "tap",
            "wg",
            "tailscale",
            "sit",
            "ip6tnl",
            "gre",
            "gretap",
            "erspan",
            "geneve",
            "vxlan",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn network_summary_line(
    interface: &NetworkInterfaceInfo,
    model: Option<&str>,
    width: usize,
) -> Line<'static> {
    let (prefix, state, suffix) = network_summary_parts(interface, model, width);
    let (_, state_style) = super::network::state_display(interface.operstate);
    Line::from(vec![
        Span::raw(prefix),
        Span::styled(state, state_style),
        Span::raw(suffix),
    ])
}

fn network_summary_parts(
    interface: &NetworkInterfaceInfo,
    model: Option<&str>,
    width: usize,
) -> (String, String, String) {
    let (state_text, _) = super::network::state_display(interface.operstate);
    let state = layout::truncate(state_text, width);
    let separator_width = usize::from(width > state.chars().count()) * 2;
    let name_width = width
        .saturating_sub(state.chars().count() + separator_width)
        .min(16);
    let name = layout::truncate(&interface.name, name_width);
    let mut prefix = if name.is_empty() {
        String::new()
    } else {
        format!("{name}  ")
    };

    let traffic = network_traffic(interface).and_then(|traffic| {
        let full = format!("  RX {}  TX {}", traffic.0, traffic.1);
        let compact = format!("  R {}  T {}", traffic.0, traffic.1);
        let tight = format!(
            "  R{} T{}",
            format_rate_tight(interface.rx_rate_bytes_per_sec),
            format_rate_tight(interface.tx_rate_bytes_per_sec)
        );
        let base_width = prefix.chars().count() + state.chars().count();
        if base_width + full.chars().count() <= width {
            Some(full)
        } else if base_width + compact.chars().count() <= width {
            Some(compact)
        } else if base_width + tight.chars().count() <= width {
            Some(tight)
        } else {
            None
        }
    });
    let suffix = traffic.unwrap_or_default();

    if let Some(model) = model {
        let occupied = prefix.chars().count() + state.chars().count() + suffix.chars().count();
        let model_width = width.saturating_sub(occupied + 2);
        if model_width >= 4 {
            prefix.push_str(&layout::truncate(model, model_width));
            prefix.push_str("  ");
        }
    }

    (prefix, state, suffix)
}

fn network_traffic(interface: &NetworkInterfaceInfo) -> Option<(String, String)> {
    let has_traffic = [
        interface.rx_rate_bytes_per_sec,
        interface.tx_rate_bytes_per_sec,
    ]
    .into_iter()
    .flatten()
    .any(|rate| rate >= 1.0);
    if interface.operstate != OperState::Up && !has_traffic {
        return None;
    }
    Some((
        super::network::format_rate(interface.rx_rate_bytes_per_sec),
        super::network::format_rate(interface.tx_rate_bytes_per_sec),
    ))
}

fn format_rate_tight(rate: Option<f64>) -> String {
    const UNITS: [&str; 5] = ["B/s", "K/s", "M/s", "G/s", "T/s"];
    let Some(mut value) = rate else {
        return "--".into();
    };
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 10.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
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

fn format_usage_compact(used: u64, total: u64) -> String {
    format!("{} / {}", format_bytes(used), format_bytes(total))
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
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;

    fn test_interface(name: &str, state: OperState) -> NetworkInterfaceInfo {
        NetworkInterfaceInfo {
            name: name.into(),
            operstate: state,
            mac_address: None,
            mtu: None,
            ipv4_addresses: Vec::new(),
            ipv6_addresses: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            rx_packets: 0,
            tx_packets: 0,
            rx_errors: 0,
            tx_errors: 0,
            rx_dropped: 0,
            tx_dropped: 0,
            rx_rate_bytes_per_sec: Some(1.2 * 1024.0 * 1024.0),
            tx_rate_bytes_per_sec: Some(84.2 * 1024.0),
        }
    }

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
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 4);
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
        assert_eq!(grid.visible, 112);
        assert!(grid.visible < 128);
        assert!(grid.rows <= 16);
    }

    #[test]
    fn narrow_and_tiny_cpu_grids_remain_bounded() {
        let narrow = cpu_grid_layout(12, 11, 9, 4);
        assert_eq!(narrow.columns, 1);
        assert_eq!(narrow.rows, 4);
        assert_eq!(narrow.visible, 4);
        assert!(narrow.visible < 12);

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
    fn cpu_summary_uses_available_vertical_breathing_room() {
        let mut app = App::default();
        app.update(crate::action::Action::SystemMetricsUpdated(SystemMetrics {
            cpu_percent: Some(12.0),
            logical_cpus: vec![LogicalCpuMetrics {
                id: crate::linux::LogicalCpuId::for_test(0),
                utilization_percent: Some(8.0),
            }],
            ..SystemMetrics::default()
        }));
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_cpu(
                    frame,
                    &app,
                    None,
                    app.system_metrics(),
                    cpu_grid_layout(1, 0, 60, 1),
                    frame.area(),
                );
            })
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(60)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();

        assert!(rows[0].starts_with("CPU"));
        assert!(rows[1].starts_with("Discovering hardware"));
        assert!(rows[2].trim().is_empty());
        assert!(rows[3].starts_with("Util"));
        assert!(rows[4].starts_with("Load"));
        assert!(rows[5].trim().is_empty());
        assert!(rows[6].starts_with("CPU0"));
    }

    #[test]
    fn wide_cpu_grid_caps_columns_and_provides_useful_gauges() {
        let grid = cpu_grid_layout(12, 11, 120, 6);
        assert_eq!(grid.columns, 4);
        assert!(!grid.dense);

        let cpus = (0..12)
            .map(|index| LogicalCpuMetrics {
                id: crate::linux::LogicalCpuId::for_test(index),
                utilization_percent: Some(48.0),
            })
            .collect::<Vec<_>>();
        let lines = cpu_grid_lines(&cpus, 120, grid);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].matches(['█', '░']).count() >= 4 * MIN_USEFUL_CPU_GAUGE_WIDTH);
        assert!(lines[2].contains("CPU10"));
        assert!(lines[2].contains("CPU11"));
    }

    #[test]
    fn medium_and_narrow_cpu_grids_prefer_readable_columns() {
        assert_eq!(cpu_grid_layout(12, 11, 72, 6).columns, 3);
        assert_eq!(cpu_grid_layout(12, 11, 44, 6).columns, 2);
    }

    #[test]
    fn logical_cpu_cells_have_an_explicit_gutter() {
        let cpus = (0..2)
            .map(|index| LogicalCpuMetrics {
                id: crate::linux::LogicalCpuId::for_test(index),
                utilization_percent: Some(50.0),
            })
            .collect::<Vec<_>>();
        let grid = cpu_grid_layout(2, 1, 44, 2);
        let line = &cpu_grid_lines(&cpus, 44, grid)[0];
        let cell_width = (44 - CPU_CELL_GAP) / 2;

        assert_eq!(line.chars().count(), 44);
        assert_eq!(
            line.chars()
                .skip(cell_width)
                .take(CPU_CELL_GAP)
                .collect::<String>(),
            "  "
        );
        assert_eq!(
            line.chars()
                .skip(cell_width + CPU_CELL_GAP)
                .take(4)
                .collect::<String>(),
            "CPU1"
        );
    }

    #[test]
    fn logical_cpu_percentages_and_gauges_align() {
        let cells = [1.0, 10.0, 100.0].map(|percent| {
            detailed_cpu_cell(
                &LogicalCpuMetrics {
                    id: crate::linux::LogicalCpuId::for_test(0),
                    utilization_percent: Some(percent),
                },
                24,
                5,
            )
        });
        let gauge_starts = cells.clone().map(|cell| {
            cell.chars()
                .position(|character| matches!(character, '█' | '░'))
                .unwrap()
        });

        assert_eq!(gauge_starts, [11, 11, 11]);
        assert!(cells[0].contains("  1%"));
        assert!(cells[1].contains(" 10%"));
        assert!(cells[2].contains("100%"));
    }

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
    fn network_summary_handles_models_rates_and_down_interfaces() {
        let up = test_interface("enp8s0", OperState::Up);
        let with_model = network_summary_parts(&up, Some("Realtek RTL8125 2.5GbE"), 100);
        let with_model = format!("{}{}{}", with_model.0, with_model.1, with_model.2);
        assert!(with_model.contains("enp8s0  Realtek RTL8125 2.5GbE  ● up"));
        assert!(with_model.contains("RX 1.2 MiB/s"));
        assert!(with_model.contains("TX 84.2 KiB/s"));

        let without_model = network_summary_parts(&up, None, 100);
        let without_model = format!("{}{}{}", without_model.0, without_model.1, without_model.2);
        assert!(without_model.starts_with("enp8s0  ● up"));

        let mut down = test_interface("wlp5s0", OperState::Down);
        down.rx_rate_bytes_per_sec = Some(0.0);
        down.tx_rate_bytes_per_sec = None;
        let down = network_summary_parts(&down, Some("Intel Wi-Fi 6E AX210"), 80);
        let down = format!("{}{}{}", down.0, down.1, down.2);
        assert!(down.contains("○ down"));
        assert!(!down.contains("RX"));
        assert!(!down.contains("TX"));
    }

    #[test]
    fn network_summary_truncates_long_models_to_available_width() {
        let interface = test_interface("enp8s0", OperState::Up);
        let parts = network_summary_parts(
            &interface,
            Some("A deliberately very long network adapter model description"),
            60,
        );
        let text = format!("{}{}{}", parts.0, parts.1, parts.2);

        assert!(text.chars().count() <= 60);
        assert!(text.contains('…'));
        assert!(text.contains("● up"));
        assert!(text.contains("RX"));
    }

    #[test]
    fn network_summary_prefers_physical_interfaces_and_filters_noise() {
        let interfaces = [
            test_interface("docker0", OperState::Up),
            test_interface("wlp5s0", OperState::Down),
            test_interface("veth1234", OperState::Up),
            test_interface("enp6s0", OperState::Up),
            test_interface("lo", OperState::Unknown),
        ];
        let inventory = HardwareInventory {
            network_devices: vec![crate::linux::NetworkDevice {
                interface_name: "enp6s0".into(),
                model: None,
            }],
            ..HardwareInventory::default()
        };

        let selected = overview_network_interfaces(&interfaces, Some(&inventory));
        let names = selected
            .iter()
            .map(|interface| interface.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["enp6s0", "wlp5s0"]);
    }

    #[test]
    fn narrow_network_summary_keeps_status_and_compact_rates() {
        let interface = test_interface("enp8s0", OperState::Up);
        let parts = network_summary_parts(&interface, Some("Realtek RTL8125 2.5GbE"), 36);
        let text = format!("{}{}{}", parts.0, parts.1, parts.2);

        assert!(text.chars().count() <= 36);
        assert!(text.starts_with("enp8s0"));
        assert!(text.contains("● up"));
        assert!(text.contains("R1.2M/s"));
        assert!(text.contains("T84K/s"));
    }

    #[test]
    fn network_section_limits_visible_interfaces() {
        let mut app = App::default();
        let interfaces = (0..5)
            .map(|index| test_interface(&format!("eth{index}"), OperState::Up))
            .collect();
        app.update(crate::action::Action::NetworkUpdated(
            crate::linux::NetworkSnapshot {
                interfaces,
                error: None,
            },
        ));
        let backend = TestBackend::new(100, 50);
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

        assert!(text.contains("eth0"));
        assert!(text.contains("eth1"));
        assert!(text.contains("eth2"));
        assert!(!text.contains("eth3"));
        assert!(text.contains("2 more interfaces"));
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
