use std::{path::Path, sync::Arc};

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::App,
    linux::{
        GpuDevice, GpuKind, HardwareInventory, MemoryModule, StorageDevice, StorageKind,
        SystemMetrics, Temperature, TemperatureKey,
    },
};

use super::{
    cards::{self, CardGraph, CardTitle, TitleOrder},
    format_bytes, hardware_network_summary, layout, network,
    overview::CardHeight,
    theme,
};

const MAX_RAM_GAUGE_WIDTH: usize = 36;
/// Memory modules listed under the RAM usage when there is room.
const MAX_MODULE_ROWS: u16 = 2;
/// A temperature after a row's text is dropped rather than leave the text
/// fewer columns than this.
const MIN_TEXT_BEFORE_TEMPERATURE: usize = 8;
/// Storage cards this wide list mounts and devices side by side.
const STORAGE_SIDE_BY_SIDE_WIDTH: u16 = 90;

fn module_rows(app: &App) -> u16 {
    app.hardware()
        .map_or(0, |inventory| inventory.memory_modules.len() as u16)
        .min(MAX_MODULE_ROWS)
}

pub(super) fn memory_card_height(app: &App, graphs: bool) -> CardHeight {
    CardHeight::new(1, module_rows(app), graphs)
}

/// Memory: a graph of RAM use, the RAM gauge, and the modules if known.
pub(super) fn render_memory_card(frame: &mut Frame, app: &App, area: Rect, graphs: bool) {
    let graph = CardGraph {
        history: app.memory_history(),
        scale: 100.0,
        interval: app.cpu_history_interval(),
    };
    let (rows, module_limit) = cards::render_graph_card(
        frame,
        area,
        &CardTitle::plain("Memory"),
        Some(graph),
        graphs,
        1,
        module_rows(app),
    );
    if rows.width == 0 || rows.height == 0 {
        return;
    }
    let width = usize::from(rows.width);
    let usage = app.system_metrics().memory.map_or_else(
        || "RAM  N/A".into(),
        |memory| usage_bar_line("RAM  ", memory, width),
    );
    let mut lines = vec![Line::from(layout::truncate(&usage, width))];
    if let Some(inventory) = app.hardware() {
        lines.extend(
            inventory
                .memory_modules
                .iter()
                .enumerate()
                .take(usize::from(module_limit))
                .map(|(index, module)| {
                    Line::from(layout::truncate(
                        &format_memory_module(index, module),
                        width,
                    ))
                }),
        );
    }
    frame.render_widget(Paragraph::new(lines), rows);
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

/// The GPU the card is about: the first discrete one, else the first.
fn primary_gpu(inventory: &HardwareInventory) -> Option<usize> {
    inventory
        .gpus
        .iter()
        .position(|gpu| gpu.kind == Some(GpuKind::Discrete))
        .or((!inventory.gpus.is_empty()).then_some(0))
}

pub(super) fn gpu_card_height(inventory: Option<&HardwareInventory>) -> CardHeight {
    let others = inventory.map_or(0, |inventory| inventory.gpus.len().saturating_sub(1));
    CardHeight::new(1, others as u16, false)
}

/// GPU: the primary GPU in the title with its temperature; its kind and
/// VRAM, then one row per other GPU.
pub(super) fn render_gpu_card(frame: &mut Frame, app: &App, area: Rect) {
    let metrics = app.system_metrics();
    let inventory = app.hardware();
    let primary = inventory
        .and_then(|inventory| primary_gpu(inventory).map(|index| (index, &inventory.gpus[index])));
    let title = CardTitle {
        name: "GPU",
        model: primary.map_or("", |(_, gpu)| gpu.model.as_str()),
        parts: primary
            .and_then(|(_, gpu)| device_temperature(metrics, gpu.device_path.as_ref()))
            .map(|temperature| vec![vec![temperature_span(temperature)]])
            .unwrap_or_default(),
        order: TitleOrder::ModelFirst,
    };
    let others = inventory.map_or(0, |inventory| inventory.gpus.len().saturating_sub(1)) as u16;
    let (rows, other_limit) = cards::render_graph_card(frame, area, &title, None, false, 1, others);
    if rows.width == 0 || rows.height == 0 {
        return;
    }
    let width = usize::from(rows.width);
    let lines = match (inventory, primary) {
        (None, _) => vec![Line::from("Discovering hardware…")],
        (Some(_), None) => vec![Line::from("No GPU detected")],
        (Some(inventory), Some((primary, gpu))) => {
            let details = [gpu_kind(gpu), gpu_vram(gpu)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("  ");
            let mut lines = vec![Line::from(layout::truncate(&details, width))];
            let mut others: Vec<&GpuDevice> = inventory
                .gpus
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != primary)
                .map(|(_, gpu)| gpu)
                .collect();
            let overflow = others.len() > usize::from(other_limit);
            others.truncate(usize::from(other_limit.saturating_sub(u16::from(overflow))));
            lines.extend(others.iter().map(|gpu| {
                let text = [Some(gpu.model.clone()), gpu_kind(gpu)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("  ");
                line_with_temperatures(
                    &text,
                    device_temperature(metrics, gpu.device_path.as_ref())
                        .map(|temperature| vec![temperature_span(temperature)]),
                    width,
                )
            }));
            if overflow {
                lines.push(Line::from(format!(
                    "… {} more GPUs",
                    inventory.gpus.len() - 1 - others.len()
                )));
            }
            lines
        }
    };
    frame.render_widget(Paragraph::new(lines), rows);
}

fn gpu_kind(gpu: &GpuDevice) -> Option<String> {
    match gpu.kind {
        Some(GpuKind::Integrated) => Some("iGPU".into()),
        Some(GpuKind::Discrete) => Some("dGPU".into()),
        None => None,
    }
}

fn gpu_vram(gpu: &GpuDevice) -> Option<String> {
    gpu.vram_bytes
        .map(|bytes| format!("{} VRAM", format_binary_capacity(bytes)))
}

/// Mount rows (the root filesystem for now) and device rows.
fn storage_row_counts(
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
) -> (u16, u16) {
    let mounts = u16::from(metrics.root_filesystem.is_some());
    let devices = inventory.map_or(1, |inventory| inventory.storage_devices.len().max(1)) as u16;
    (mounts, devices)
}

pub(super) fn storage_card_height(
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
    inner_width: u16,
) -> CardHeight {
    let (mounts, devices) = storage_row_counts(inventory, metrics);
    if inner_width >= STORAGE_SIDE_BY_SIDE_WIDTH {
        CardHeight::new(1, mounts.max(devices) - 1, false)
    } else {
        CardHeight::new(1, (mounts + devices).saturating_sub(1), false)
    }
}

/// Storage: filesystem usage and the disks, side by side on a wide card and
/// one after the other otherwise.
pub(super) fn render_storage_card(frame: &mut Frame, app: &App, area: Rect) {
    let inner = cards::render_card(frame, area, &CardTitle::plain("Storage"), None);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let metrics = app.system_metrics();
    let inventory = app.hardware();
    let mount_line = |width: usize| -> Vec<Line<'static>> {
        metrics
            .root_filesystem
            .map(|usage| Line::from(usage_bar_line("/  ", usage, width)))
            .into_iter()
            .collect()
    };
    if inner.width >= STORAGE_SIDE_BY_SIDE_WIDTH {
        let mount_width = inner.width * 2 / 5;
        let gap = 2;
        let devices_area = Rect::new(
            inner.x + mount_width + gap,
            inner.y,
            inner.width - mount_width - gap,
            inner.height,
        );
        frame.render_widget(
            Paragraph::new(mount_line(usize::from(mount_width))),
            Rect::new(inner.x, inner.y, mount_width, inner.height),
        );
        let lines = device_lines(inventory, metrics, devices_area.width, devices_area.height);
        frame.render_widget(Paragraph::new(lines), devices_area);
    } else {
        let mut lines = mount_line(usize::from(inner.width));
        let height = inner.height.saturating_sub(lines.len() as u16);
        lines.extend(device_lines(inventory, metrics, inner.width, height));
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// One row per disk within `height` rows, with `… N more devices` when they
/// do not all fit.
fn device_lines(
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let height = usize::from(height);
    if height == 0 {
        return Vec::new();
    }
    let width = usize::from(width);
    match inventory {
        None => vec![Line::from("Discovering hardware…")],
        Some(inventory) if inventory.storage_devices.is_empty() => {
            vec![Line::from("No disks detected")]
        }
        Some(inventory) => {
            let devices = &inventory.storage_devices;
            let overflow = devices.len() > height;
            let limit = if overflow { height - 1 } else { devices.len() };
            let mut counts = [0_usize; 5];
            let rows: Vec<_> = devices
                .iter()
                .take(limit)
                .map(|device| {
                    let (label, count_index) = storage_label(device.kind);
                    let index = counts[count_index];
                    counts[count_index] += 1;
                    StorageRow {
                        description: format_storage_device(label, index, device),
                        rates: disk_rates(metrics, &device.system_name),
                        temperature: device_temperature(metrics, device.device_path.as_ref()),
                    }
                })
                .collect();
            let mut lines = storage_lines(&rows, width);
            if overflow {
                lines.push(Line::from(format!(
                    "… {} more devices",
                    devices.len() - limit
                )));
            }
            lines
        }
    }
}

/// The temperature of the device at `path`, when a sensor belongs to it.
pub(super) fn device_temperature<'a>(
    metrics: &'a SystemMetrics,
    path: Option<&Arc<Path>>,
) -> Option<&'a Temperature> {
    let path = path?;
    metrics
        .temperatures
        .iter()
        .find(|temperature| matches!(&temperature.key, TemperatureKey::Device(key) if key == path))
}

/// `45°C`, or `–` while a known sensor has no value (a suspended GPU).
pub(super) fn temperature_text(temperature: &Temperature) -> String {
    temperature
        .celsius
        .map_or_else(|| "–".into(), |celsius| format!("{celsius}°C"))
}

/// Highlighted only when the driver reports a limit and the value reaches it.
pub(super) fn temperature_style(temperature: &Temperature) -> Style {
    if temperature.at_limit() {
        Style::default().fg(theme::WARNING)
    } else {
        Style::default()
    }
}

pub(super) fn temperature_span(temperature: &Temperature) -> Span<'static> {
    Span::styled(
        temperature_text(temperature),
        temperature_style(temperature),
    )
}

/// `text  <temperatures>`: the text is truncated first; the temperatures are
/// dropped when the text would keep fewer than
/// [`MIN_TEXT_BEFORE_TEMPERATURE`] columns.
pub(super) fn line_with_temperatures(
    text: &str,
    temperatures: Option<Vec<Span<'static>>>,
    width: usize,
) -> Line<'static> {
    let Some(temperatures) = temperatures.filter(|spans| !spans.is_empty()) else {
        return Line::from(layout::truncate(text, width));
    };
    let suffix_width = 2 + temperatures
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let Some(room) = width
        .checked_sub(suffix_width)
        .filter(|room| *room >= MIN_TEXT_BEFORE_TEMPERATURE.min(text.chars().count()))
    else {
        return Line::from(layout::truncate(text, width));
    };
    let mut spans = vec![Span::raw(format!("{}  ", layout::truncate(text, room)))];
    spans.extend(temperatures);
    Line::from(spans)
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

type DiskRates = Option<(Option<f64>, Option<f64>)>;

/// A storage row before layout.
struct StorageRow<'a> {
    description: String,
    rates: DiskRates,
    temperature: Option<&'a Temperature>,
}

/// Storage rows: the description, a temperature column and the rates, each
/// column right after the longest entry of the one before, so the numbers
/// stay next to the device they belong to on a wide panel. As the panel
/// narrows, the models are truncated first, then the rates switch to a tight
/// form, then they are left out; the temperatures go last, when not even the
/// device labels would remain beside them.
fn storage_lines(rows: &[StorageRow], width: usize) -> Vec<Line<'static>> {
    let plain = || {
        rows.iter()
            .map(|row| Line::from(layout::truncate(&row.description, width)))
            .collect()
    };
    let label_width = rows
        .iter()
        .map(|row| {
            row.description
                .split("  ")
                .next()
                .unwrap_or_default()
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0);
    let description_width = rows
        .iter()
        .map(|row| row.description.chars().count())
        .max()
        .unwrap_or(0);
    let temperatures: Vec<Option<Span<'static>>> = rows
        .iter()
        .map(|row| row.temperature.map(temperature_span))
        .collect();
    let temperature_width = temperatures
        .iter()
        .flatten()
        .map(|span| span.content.chars().count())
        .max();
    let temperature_column = temperature_width.map_or(0, |width| 2 + width);
    let has_rates = rows.iter().any(|row| row.rates.is_some());

    type Format = fn(Option<f64>) -> String;
    let full: (Format, &str, &str) = (network::format_rate, "  R ", "  W ");
    let tight: (Format, &str, &str) = (hardware_network_summary::format_rate_tight, "  R", " W");
    // The rate forms, widest first; `None` leaves the rates out.
    for form in [Some(full), Some(tight), None] {
        if (form.is_some() && !has_rates) || (form.is_none() && temperature_width.is_none()) {
            continue;
        }
        let texts: Vec<Option<(String, String)>> = rows
            .iter()
            .map(|row| {
                let (format, ..) = form?;
                row.rates.map(|(read, write)| (format(read), format(write)))
            })
            .collect();
        let widest = |text: fn(&(String, String)) -> &String| {
            texts
                .iter()
                .flatten()
                .map(|pair| text(pair).chars().count())
                .max()
                .unwrap_or(0)
        };
        let read_column = widest(|(read, _)| read);
        let (read_prefix, write_prefix) = form.map_or(("", ""), |(_, read, write)| (read, write));
        let rates_width = form.map_or(0, |_| {
            read_prefix.len() + read_column + write_prefix.len() + widest(|(_, write)| write)
        });
        let Some(room) = width.checked_sub(rates_width + temperature_column) else {
            continue;
        };
        // Truncating a longer description takes a column for `…`; the
        // labels themselves must stay whole.
        if room < label_width + usize::from(description_width > label_width) {
            continue;
        }
        let column = description_width.min(room);
        return rows
            .iter()
            .zip(texts)
            .zip(&temperatures)
            .map(|((row, rates), temperature)| {
                if rates.is_none() && temperature.is_none() {
                    return Line::from(layout::truncate(&row.description, width));
                }
                let mut spans = vec![Span::raw(format!(
                    "{:<column$}",
                    layout::truncate(&row.description, column)
                ))];
                if let Some(temperature_width) = temperature_width {
                    let shown = temperature
                        .as_ref()
                        .map_or(0, |span| span.content.chars().count());
                    spans.push(Span::raw("  "));
                    spans.extend(temperature.clone());
                    if rates.is_some() {
                        spans.push(Span::raw(" ".repeat(temperature_width - shown)));
                    }
                }
                if let Some((read, write)) = rates {
                    spans.push(Span::raw(format!(
                        "{read_prefix}{read:<read_column$}{write_prefix}{write}"
                    )));
                }
                Line::from(spans)
            })
            .collect();
    }
    plain()
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

    fn rows(app: &App, width: u16, height: u16, draw: fn(&mut Frame, &App, Rect)) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn memory_app() -> App {
        let mut app = App::default();
        for used in [8_u64, 16, 24] {
            app.update(crate::action::Action::SystemMetricsUpdated(SystemMetrics {
                memory: Some(crate::linux::ByteUsage {
                    used: used << 30,
                    total: 32 << 30,
                }),
                ..SystemMetrics::default()
            }));
        }
        app
    }

    #[test]
    fn the_memory_card_graphs_ram_use_above_its_gauge() {
        let app = memory_app();
        let card = rows(&app, 60, 8, |frame, app, area| {
            render_memory_card(frame, app, area, true)
        });
        assert!(card[0].contains(" Memory "), "{card:#?}");
        // Five graph rows: 75 % of 40 eighths is three full rows and a ▆.
        assert!(
            card[2].ends_with("▆│"),
            "newest sample on the right: {card:#?}"
        );
        assert!(card[5].ends_with("███│"), "{card:#?}");
        assert!(card[6].contains("RAM  75%"), "{card:#?}");
        assert!(card[7].contains(" 58s "), "58 samples fit: {card:#?}");

        // Compact: the graph moves into the title row.
        let compact = rows(&app, 60, 3, |frame, app, area| {
            render_memory_card(frame, app, area, false)
        });
        assert!(
            compact[0].contains(" Memory ") && compact[0].contains('▆'),
            "{compact:#?}"
        );
        assert!(compact[1].contains("RAM  75%"));
    }

    fn gpu(model: &str, kind: Option<GpuKind>) -> GpuDevice {
        GpuDevice {
            model: model.into(),
            kind,
            vram_bytes: Some(16 << 30),
            device_path: None,
        }
    }

    #[test]
    fn the_gpu_card_is_about_the_discrete_gpu_and_lists_the_others() {
        let mut app = App::default();
        let card = |app: &App| {
            rows(app, 60, 6, |frame, app, area| {
                render_gpu_card(frame, app, area)
            })
        };
        assert!(card(&app)[1].contains("Discovering hardware…"));

        app.update(crate::action::Action::HardwareDiscovered(
            HardwareInventory::default(),
        ));
        assert!(card(&app)[1].contains("No GPU detected"));

        app.update(crate::action::Action::HardwareDiscovered(
            HardwareInventory {
                gpus: vec![
                    gpu("Intel UHD Graphics 770", Some(GpuKind::Integrated)),
                    gpu("NVIDIA GeForce RTX 5070 Ti", Some(GpuKind::Discrete)),
                ],
                ..HardwareInventory::default()
            },
        ));
        let rows = card(&app);
        assert!(
            rows[0].contains(" GPU  NVIDIA GeForce RTX 5070 Ti "),
            "{rows:#?}"
        );
        assert!(rows[1].contains("dGPU  16 GiB VRAM"), "{rows:#?}");
        assert!(
            rows[2].contains("Intel UHD Graphics 770  iGPU"),
            "{rows:#?}"
        );
    }

    #[test]
    fn storage_lists_mounts_and_disks_side_by_side_on_a_wide_card() {
        let mut app = App::default();
        app.update(crate::action::Action::SystemMetricsUpdated(SystemMetrics {
            root_filesystem: Some(crate::linux::ByteUsage {
                used: 50 << 30,
                total: 100 << 30,
            }),
            ..SystemMetrics::default()
        }));
        app.update(crate::action::Action::HardwareDiscovered(
            HardwareInventory {
                storage_devices: vec![StorageDevice {
                    system_name: "nvme0n1".into(),
                    kind: StorageKind::Nvme,
                    model: Some("Test Disk".into()),
                    capacity_bytes: Some(1_000_000_000_000),
                    device_path: None,
                }],
                ..HardwareInventory::default()
            },
        ));
        let draw: fn(&mut Frame, &App, Rect) =
            |frame, app, area| render_storage_card(frame, app, area);

        let wide = rows(&app, 120, 4, draw);
        assert!(wide[0].contains(" Storage "));
        assert!(
            wide[1].contains("/  50%") && wide[1].contains("NVMe0  Test Disk"),
            "{wide:#?}"
        );

        let narrow = rows(&app, 60, 5, draw);
        assert!(narrow[1].contains("/  50%"), "{narrow:#?}");
        assert!(narrow[2].contains("NVMe0  Test Disk"), "{narrow:#?}");
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

    const MIB: f64 = 1024.0 * 1024.0;

    fn line_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Storage rows without temperatures, laid out as plain text.
    fn storage_text(rows: &[(String, DiskRates)], width: usize) -> Vec<String> {
        let rows: Vec<StorageRow> = rows
            .iter()
            .map(|(description, rates)| StorageRow {
                description: description.clone(),
                rates: *rates,
                temperature: None,
            })
            .collect();
        storage_lines(&rows, width).iter().map(line_text).collect()
    }

    fn two_disks() -> Vec<(String, DiskRates)> {
        vec![
            (
                "NVMe0  WD Blue SN5100 1TB  1.0 TB".into(),
                Some((Some(12.3 * MIB), Some(0.0))),
            ),
            (
                "SATA0  Samsung SSD 870  2.0 TB".into(),
                Some((Some(0.0), Some(512.0 * 1024.0))),
            ),
        ]
    }

    #[test]
    fn storage_rates_follow_the_longest_description_on_a_wide_panel() {
        let lines = storage_text(&two_disks(), 120);

        assert_eq!(
            lines,
            [
                "NVMe0  WD Blue SN5100 1TB  1.0 TB  R 12.3 MiB/s  W 0 B/s",
                "SATA0  Samsung SSD 870  2.0 TB     R 0 B/s       W 512.0 KiB/s",
            ],
            "rates sit after the descriptions, with R and W aligned"
        );
    }

    #[test]
    fn narrow_storage_rows_truncate_models_then_tighten_then_drop_rates() {
        let rows = two_disks();

        let medium = storage_text(&rows, 50);
        assert!(
            medium.iter().all(|line| line.chars().count() <= 50),
            "{medium:?}"
        );
        assert!(medium[0].starts_with("NVMe0  WD Blue"), "{medium:?}");
        assert!(medium[0].contains("…  R 12.3 MiB/s  W 0 B/s"), "{medium:?}");
        let w = |line: &str| line.find("  W ").unwrap();
        assert_eq!(w(&medium[0]), w(&medium[1]), "W stays aligned");

        let narrow = storage_text(&rows, 24);
        assert!(
            narrow.iter().all(|line| line.chars().count() <= 24),
            "{narrow:?}"
        );
        assert!(narrow[0].starts_with("NVMe0"), "{narrow:?}");
        assert!(narrow[0].ends_with("  R12M/s W0B/s"), "{narrow:?}");

        let tiny = storage_text(&rows, 8);
        assert_eq!(tiny[0], layout::truncate(&rows[0].0, 8), "rates dropped");
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
        let missing = [("SATA0  disk".to_owned(), disk_rates(&metrics, "nvme0n1"))];
        assert_eq!(storage_text(&missing, 40), ["SATA0  disk  R --  W --"]);

        metrics.disks.clear();
        let hidden = [("SATA0  disk".to_owned(), disk_rates(&metrics, "sda"))];
        assert_eq!(storage_text(&hidden, 40), ["SATA0  disk"]);
    }

    fn temperature(celsius: Option<i16>, max: Option<i16>) -> Temperature {
        Temperature {
            key: TemperatureKey::CpuPackage(0),
            celsius,
            max,
            crit: None,
        }
    }

    #[test]
    fn storage_temperatures_form_a_column_before_the_rates() {
        let hot = temperature(Some(45), None);
        let rows = [
            StorageRow {
                description: "NVMe0  WD Blue SN5100 1TB  1.0 TB".into(),
                rates: Some((Some(12.3 * MIB), Some(0.0))),
                temperature: Some(&hot),
            },
            StorageRow {
                description: "SATA0  Samsung SSD 870  2.0 TB".into(),
                rates: Some((Some(0.0), Some(512.0 * 1024.0))),
                temperature: None,
            },
        ];

        let wide: Vec<String> = storage_lines(&rows, 120).iter().map(line_text).collect();
        assert_eq!(
            wide,
            [
                "NVMe0  WD Blue SN5100 1TB  1.0 TB  45°C  R 12.3 MiB/s  W 0 B/s",
                "SATA0  Samsung SSD 870  2.0 TB           R 0 B/s       W 512.0 KiB/s",
            ],
            "a disk without a sensor leaves the column blank"
        );

        // Narrowing: models truncate, rates tighten, rates go, then the temperature.
        let mut stage = 0;
        for width in (0..=120).rev() {
            let lines: Vec<String> = storage_lines(&rows, width).iter().map(line_text).collect();
            assert!(
                lines.iter().all(|line| line.chars().count() <= width),
                "{width}: {lines:?}"
            );
            let current = if lines[0].contains("  R 12.3") {
                0
            } else if lines[0].contains("  R12M/s") {
                1
            } else if lines[0].contains("45°C") {
                2
            } else {
                3
            };
            assert!(
                current >= stage,
                "{width}: stage {current} after {stage}: {lines:?}"
            );
            stage = current;
            if current <= 2 {
                assert!(lines[0].contains("45°C"), "{width}: {lines:?}");
                assert!(lines[0].starts_with("NVMe0"), "{width}: {lines:?}");
            }
        }
        assert_eq!(stage, 3, "the temperature is dropped last");

        let rates_gone: Vec<String> = storage_lines(&rows, 16).iter().map(line_text).collect();
        assert_eq!(rates_gone[0], "NVMe0  WD…  45°C");
        assert_eq!(
            rates_gone[1],
            "SATA0  Samsung SSD 870  2.0 TB"
                .chars()
                .take(15)
                .collect::<String>()
                + "…"
        );
    }

    #[test]
    fn storage_temperatures_without_disk_statistics_and_unavailable_sensors() {
        let asleep = temperature(None, None);
        let rows = [StorageRow {
            description: "NVMe0  disk".into(),
            rates: None,
            temperature: Some(&asleep),
        }];
        let lines: Vec<String> = storage_lines(&rows, 40).iter().map(line_text).collect();
        assert_eq!(lines, ["NVMe0  disk  –"]);
    }

    #[test]
    fn temperatures_are_highlighted_only_at_a_driver_limit() {
        assert_eq!(
            temperature_style(&temperature(Some(99), None)),
            Style::default()
        );
        assert_eq!(
            temperature_style(&temperature(Some(79), Some(80))),
            Style::default()
        );
        assert_eq!(
            temperature_style(&temperature(Some(80), Some(80))),
            Style::default().fg(theme::WARNING)
        );
        assert_eq!(temperature_text(&temperature(Some(-5), None)), "-5°C");
        assert_eq!(temperature_text(&temperature(None, None)), "–");
    }

    #[test]
    fn row_temperatures_outlast_the_text_until_it_would_be_too_short() {
        let spans = || Some(vec![temperature_span(&temperature(Some(52), None))]);
        let model = "NVIDIA GeForce RTX 4070  dGPU  12 GiB VRAM";
        assert_eq!(
            line_text(&line_with_temperatures(model, spans(), 80)),
            format!("{model}  52°C")
        );
        let narrow = line_text(&line_with_temperatures(model, spans(), 20));
        assert_eq!(narrow, "NVIDIA GeForc…  52°C");
        // Fewer than 8 columns would remain for the model: the temperature goes.
        let tiny = line_text(&line_with_temperatures(model, spans(), 13));
        assert_eq!(tiny, layout::truncate(model, 13));
        assert_eq!(line_text(&line_with_temperatures(model, None, 13)), tiny);
        assert_eq!(line_text(&line_with_temperatures(model, spans(), 0)), "");
    }

    #[test]
    fn long_storage_names_can_be_safely_truncated() {
        let device = StorageDevice {
            system_name: "nvme0n1".into(),
            model: Some("A very long storage model name that exceeds the panel".into()),
            capacity_bytes: Some(1_000_000_000_000),
            kind: StorageKind::Nvme,
            device_path: None,
        };
        let line = layout::truncate(&format_storage_device("NVMe", 0, &device), 24);

        assert_eq!(line.chars().count(), 24);
        assert!(line.ends_with('…'));
    }
}
