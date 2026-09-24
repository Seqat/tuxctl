use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::App,
    linux::{LogicalCpuMetrics, SystemMetrics, TemperatureKey},
};

use super::{
    cards::{self, CardGraph, CardTitle, TitleOrder},
    hardware::{band_style, bar_spans, format_watts, temperature_span, CPU_DEFAULT_LIMIT},
    layout,
    overview::CardHeight,
};

const PREFERRED_CPU_CELL_WIDTH: usize = 20;
const MIN_DETAILED_CPU_CELL_WIDTH: usize = 16;
const MAX_DETAILED_CPU_COLUMNS: usize = 4;
const MIN_USEFUL_CPU_GAUGE_WIDTH: usize = 6;
const CPU_CELL_GAP: usize = 2;
/// Dense cells already separate label and value, so one space between cells suffices.
const DENSE_CPU_CELL_GAP: usize = 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CpuGridLayout {
    columns: usize,
    rows: usize,
    visible: usize,
    dense: bool,
}

fn grid_layout(metrics: &SystemMetrics, width: usize, max_rows: usize) -> CpuGridLayout {
    let max_cpu_id = metrics
        .logical_cpus
        .iter()
        .map(|cpu| cpu.id.index())
        .max()
        .unwrap_or(0);
    cpu_grid_layout(metrics.logical_cpus.len(), max_cpu_id, width, max_rows)
}

/// Rows the grid needs to show every CPU at `width`.
fn full_grid_rows(metrics: &SystemMetrics, width: u16) -> u16 {
    let grid = grid_layout(metrics, usize::from(width), metrics.logical_cpus.len());
    u16::try_from(grid.rows).unwrap_or(u16::MAX)
}

/// The grid within `rows`, keeping the last one for `… N more logical CPUs`
/// whenever not every CPU fits.
fn grid_for_rows(metrics: &SystemMetrics, width: usize, rows: usize) -> CpuGridLayout {
    let grid = grid_layout(metrics, width, rows);
    if grid.visible < metrics.logical_cpus.len() {
        grid_layout(metrics, width, rows.saturating_sub(1))
    } else {
        grid
    }
}

pub(super) fn card_height(metrics: &SystemMetrics, inner_width: u16, graphs: bool) -> CardHeight {
    CardHeight::new(
        1,
        full_grid_rows(metrics, cards::padded_width(inner_width)),
        graphs,
    )
}

/// CPU: the model and package temperature in the title, a graph of total
/// utilization, utilization and load, then the per-CPU grid, which is the
/// first thing to go when the card is short.
pub(super) fn render_card(frame: &mut Frame, app: &App, area: Rect, graphs: bool) {
    let metrics = app.system_metrics();
    let title = CardTitle {
        name: "CPU",
        model: app
            .hardware()
            .and_then(|inventory| inventory.cpus.first())
            .map_or("", |cpu| cpu.model.as_str()),
        parts: package_temperatures(metrics)
            .into_iter()
            .chain(
                metrics
                    .cpu_power_watts
                    .map(|watts| vec![Span::raw(format_watts(watts))]),
            )
            .collect(),
        order: TitleOrder::ModelFirst,
    };
    let graph = CardGraph {
        history: app.aggregate_cpu_history(),
        scale: 100.0,
        interval: app.cpu_history_interval(),
        banded: true,
    };
    let grid_rows = full_grid_rows(metrics, cards::padded_width(area.width.saturating_sub(2)));
    let (rows, grid_limit) =
        cards::render_graph_card(frame, area, &title, Some(graph), graphs, 1, grid_rows);
    if rows.width == 0 || rows.height == 0 {
        return;
    }
    let width = usize::from(rows.width);
    let mut lines = vec![summary_line(metrics, width)];
    let grid = grid_for_rows(metrics, width, usize::from(grid_limit));
    lines.extend(cpu_grid_lines(&metrics.logical_cpus, width, grid));
    if grid_limit > 0 && grid.visible < metrics.logical_cpus.len() {
        lines.push(Line::from(format!(
            "… {} more logical CPUs",
            metrics.logical_cpus.len() - grid.visible
        )));
    }
    frame.render_widget(Paragraph::new(lines), rows);
}

/// `Util  12%   Load  1m 0.52  5m 0.44  15m 0.29`, or a compact form; the
/// utilization takes its band.
fn summary_line(metrics: &SystemMetrics, width: usize) -> Line<'static> {
    let percent = format_percent(metrics.cpu_percent);
    let style = band_style(metrics.cpu_percent, true);
    let (full_load, compact_load) = metrics.load_average.map_or_else(
        || ("   Load  N/A".to_owned(), "  Load N/A".to_owned()),
        |load| {
            (
                format!(
                    "   Load  1m {:.2}  5m {:.2}  15m {:.2}",
                    load.one, load.five, load.fifteen
                ),
                format!(
                    "  Load {:.2} {:.2} {:.2}",
                    load.one, load.five, load.fifteen
                ),
            )
        },
    );
    let full = Line::from(vec![
        Span::raw("Util "),
        Span::styled(format!("{percent:>4}"), style),
        Span::raw(full_load),
    ]);
    if full.width() <= width {
        return full;
    }
    let compact = Line::from(vec![
        Span::raw("Util "),
        Span::styled(percent.clone(), style),
        Span::raw(compact_load.clone()),
    ]);
    if compact.width() <= width {
        compact
    } else {
        Line::from(layout::truncate(
            &format!("Util {percent}{compact_load}"),
            width,
        ))
    }
}

/// `54°C` for one package, `P0 54°C  P1 56°C` for several; `None` without
/// a CPU sensor.
fn package_temperatures(metrics: &SystemMetrics) -> Option<Vec<Span<'static>>> {
    let mut packages: Vec<_> = metrics
        .temperatures
        .iter()
        .filter_map(|temperature| match temperature.key {
            TemperatureKey::CpuPackage(package) => Some((package, temperature)),
            TemperatureKey::Device(_) => None,
        })
        .collect();
    packages.sort_by_key(|(package, _)| *package);
    match packages.as_slice() {
        [] => None,
        [(_, temperature)] => Some(vec![temperature_span(temperature, CPU_DEFAULT_LIMIT)]),
        _ => Some(
            packages
                .iter()
                .enumerate()
                .flat_map(|(index, (package, temperature))| {
                    let separator = if index == 0 { "" } else { "  " };
                    [
                        Span::raw(format!("{separator}P{package} ")),
                        temperature_span(temperature, CPU_DEFAULT_LIMIT),
                    ]
                })
                .collect(),
        ),
    }
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
    let dense_cell_width = dense_cell_width(format!("CPU{max_cpu_id}").chars().count());
    let dense_columns = columns_that_fit(width, dense_cell_width, DENSE_CPU_CELL_GAP)
        .max(1)
        .min(count);
    // Dense cells drop the gauge, so they are only worth it when they add columns.
    let dense = needed_columns > detailed_capacity && dense_columns > detailed_capacity;
    let columns = if dense {
        dense_columns
    } else if needed_columns > detailed_capacity {
        detailed_capacity
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

/// `CPU<n> <level><percent>`: the label is always followed by a space.
const fn dense_cell_width(label_width: usize) -> usize {
    label_width + 6
}

fn cpu_columns_that_fit(width: usize, cell_width: usize) -> usize {
    columns_that_fit(width, cell_width, CPU_CELL_GAP)
}

fn columns_that_fit(width: usize, cell_width: usize, gap: usize) -> usize {
    width.saturating_add(gap) / cell_width.saturating_add(gap)
}

fn cpu_grid_lines(
    cpus: &[LogicalCpuMetrics],
    width: usize,
    grid: CpuGridLayout,
) -> Vec<Line<'static>> {
    if grid.columns == 0 || grid.visible == 0 {
        return Vec::new();
    }

    let gap_width = if grid.dense {
        DENSE_CPU_CELL_GAP
    } else {
        CPU_CELL_GAP
    };
    let total_gap = gap_width.saturating_mul(grid.columns.saturating_sub(1));
    let cell_width = width.saturating_sub(total_gap) / grid.columns;
    let label_width = cpus[..grid.visible]
        .iter()
        .map(|cpu| format!("CPU{}", cpu.id.index()).chars().count())
        .max()
        .unwrap_or(3);
    // Column-major: CPU0, CPU1, … run down the first column.
    let rows = grid.visible.div_ceil(grid.columns);
    (0..rows)
        .map(|row| {
            let mut spans = Vec::new();
            let cells = (0..grid.columns)
                .filter_map(|column| cpus[..grid.visible].get(column * rows + row));
            for (index, cpu) in cells.enumerate() {
                if index > 0 {
                    spans.push(Span::raw(" ".repeat(gap_width)));
                }
                let cell = if grid.dense {
                    dense_cpu_cell(cpu, cell_width, label_width)
                } else {
                    detailed_cpu_cell(cpu, cell_width, label_width)
                };
                spans.extend(fit_cell(cell, cell_width));
            }
            Line::from(spans)
        })
        .collect()
}

/// A cell's spans padded to exactly `width`; a cell that would not fit
/// becomes its plain text, truncated.
fn fit_cell(cell: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let used: usize = cell.iter().map(Span::width).sum();
    if used > width {
        let text: String = cell.iter().map(|span| span.content.as_ref()).collect();
        return vec![Span::raw(pad_cell(&layout::truncate(&text, width), width))];
    }
    let mut cell = cell;
    cell.push(Span::raw(" ".repeat(width - used)));
    cell
}

fn detailed_cpu_cell(
    cpu: &LogicalCpuMetrics,
    cell_width: usize,
    label_width: usize,
) -> Vec<Span<'static>> {
    let label = format!("CPU{}", cpu.id.index());
    let percent = format_percent(cpu.utilization_percent);
    let style = band_style(cpu.utilization_percent, true);
    let mut spans = vec![
        Span::raw(format!("{label:<label_width$} ")),
        Span::styled(format!("{percent:>4}"), style),
    ];
    let gauge_width = cell_width.saturating_sub(label_width + 6);
    if gauge_width >= MIN_USEFUL_CPU_GAUGE_WIDTH {
        spans.push(Span::raw(" "));
        spans.extend(bar_spans(cpu.utilization_percent, gauge_width, style));
    }
    spans
}

fn dense_cpu_cell(
    cpu: &LogicalCpuMetrics,
    cell_width: usize,
    label_width: usize,
) -> Vec<Span<'static>> {
    let label = format!("CPU{}", cpu.id.index());
    let percent = format_percent(cpu.utilization_percent);
    let style = band_style(cpu.utilization_percent, true);
    let mut spans = vec![Span::raw(format!("{label:<label_width$} "))];
    // The level glyph needs a column more; without it the value stays.
    if cell_width >= dense_cell_width(label_width) {
        spans.push(Span::styled(
            utilization_level(cpu.utilization_percent).to_string(),
            style,
        ));
    }
    spans.push(Span::styled(format!("{percent:>4}"), style));
    spans
}

/// Compact span: `15s`, `90s`, `2m`, `2m30s`, `1h`; sub-second parts as `4.8s`.
pub(super) fn format_window(window: std::time::Duration) -> String {
    let millis = window.as_millis();
    let seconds = window.as_secs();
    if !millis.is_multiple_of(1000) {
        format!("{:.1}s", window.as_secs_f64())
    } else if seconds < 120 {
        format!("{seconds}s")
    } else if seconds.is_multiple_of(3600) {
        format!("{}h", seconds / 3600)
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{}m{}s", seconds / 60, seconds % 60)
    }
}

fn utilization_level(percent: Option<f64>) -> char {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    percent.map_or('·', |percent| {
        let index = ((percent.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize;
        LEVELS[index.min(LEVELS.len() - 1)]
    })
}

fn format_percent(percent: Option<f64>) -> String {
    percent
        .map(|percent| format!("{:.0}%", percent.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "N/A".into())
}

fn pad_cell(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(padding))
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

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
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 4);
        assert_eq!(grid.visible, 12);
        assert!(!grid.dense);
    }

    #[test]
    fn thirty_two_logical_cpus_switch_to_dense_cells() {
        let grid = cpu_grid_layout(32, 31, 52, 10);
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
    fn history_windows_are_compact() {
        use std::time::Duration;
        for (window, text) in [
            (Duration::from_millis(15_000), "15s"),
            (Duration::from_secs(60), "60s"),
            (Duration::from_secs(90), "90s"),
            (Duration::from_secs(120), "2m"),
            (Duration::from_secs(150), "2m30s"),
            (Duration::from_secs(300), "5m"),
            (Duration::from_secs(3600), "1h"),
            (Duration::from_millis(4750), "4.8s"),
        ] {
            assert_eq!(format_window(window), text);
        }
    }

    fn cell_text(spans: Vec<Span<'static>>) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn grid_text(cpus: &[LogicalCpuMetrics], width: usize, grid: CpuGridLayout) -> Vec<String> {
        cpu_grid_lines(cpus, width, grid)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn utilization_values_and_gauges_take_their_band() {
        use ratatui::style::Color;
        let cpu = |percent| LogicalCpuMetrics {
            id: crate::linux::LogicalCpuId::for_test(0),
            utilization_percent: Some(percent),
        };
        let styles = |spans: Vec<Span<'static>>| {
            spans
                .into_iter()
                .filter_map(|span| span.style.fg)
                .collect::<Vec<_>>()
        };
        // Percentage and gauge fill share the band; the label does not.
        assert_eq!(
            styles(detailed_cpu_cell(&cpu(50.0), 24, 5)),
            [Color::Green; 2]
        );
        assert_eq!(
            styles(detailed_cpu_cell(&cpu(97.0), 24, 5)),
            [Color::Red; 2]
        );
        assert_eq!(
            styles(dense_cpu_cell(&cpu(70.0), 11, 5)),
            [Color::Yellow; 2]
        );
        let unknown = LogicalCpuMetrics {
            utilization_percent: None,
            ..cpu(0.0)
        };
        assert!(styles(detailed_cpu_cell(&unknown, 24, 5)).is_empty());

        let app = app_with(1, Vec::new());
        let summary = summary_line(app.system_metrics(), 60);
        assert_eq!(summary.spans[1].style.fg, Some(Color::Green), "Util 12%");
    }

    fn cpus(count: u32) -> Vec<LogicalCpuMetrics> {
        (0..count)
            .map(|index| LogicalCpuMetrics {
                id: crate::linux::LogicalCpuId::for_test(index),
                utilization_percent: Some(48.0),
            })
            .collect()
    }

    fn app_with(cpu_count: u32, temperatures: Vec<crate::linux::Temperature>) -> App {
        let mut app = App::default();
        app.update(crate::action::Action::HardwareDiscovered(
            crate::linux::HardwareInventory {
                cpus: vec![crate::linux::CpuPackage {
                    physical_id: Some(0),
                    model: "AMD Ryzen 9 7950X 16-Core Processor".into(),
                }],
                ..crate::linux::HardwareInventory::default()
            },
        ));
        app.update(crate::action::Action::SystemMetricsUpdated(SystemMetrics {
            cpu_percent: Some(12.0),
            logical_cpus: cpus(cpu_count),
            load_average: Some(crate::linux::LoadAverage {
                one: 0.52,
                five: 0.44,
                fifteen: 0.29,
            }),
            temperatures,
            ..SystemMetrics::default()
        }));
        app
    }

    fn card_rows(app: &App, width: u16, height: u16, graphs: bool) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_card(frame, app, frame.area(), graphs))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn package(id: u32, celsius: i16) -> crate::linux::Temperature {
        crate::linux::Temperature {
            key: TemperatureKey::CpuPackage(id),
            celsius: Some(celsius),
            max: None,
            crit: None,
        }
    }

    #[test]
    fn the_title_names_the_model_and_package_temperatures() {
        let one = card_rows(&app_with(2, vec![package(0, 54)]), 60, 12, true);
        assert!(
            one[0].contains(" CPU  AMD Ryzen 9 7950X 16-Core Processor · 54°C "),
            "{}",
            one[0]
        );
        let two = card_rows(
            &app_with(2, vec![package(1, 56), package(0, 54)]),
            70,
            12,
            true,
        );
        assert!(
            two[0].contains("Processor · P0 54°C  P1 56°C "),
            "{}",
            two[0]
        );
        let narrow = card_rows(&app_with(2, vec![package(0, 54)]), 30, 12, true);
        assert!(
            narrow[0].contains(" CPU  Ryzen 9 7950X · 54°C "),
            "{}",
            narrow[0]
        );
        let none = card_rows(&app_with(2, Vec::new()), 60, 12, true);
        assert!(
            none[0].contains("Processor ─"),
            "no sensor, no part: {}",
            none[0]
        );
    }

    #[test]
    fn the_bottom_border_states_the_time_span_of_the_graph() {
        let app = app_with(2, Vec::new());
        // 98 of the 240 samples kept fit in a 100-column card.
        assert!(card_rows(&app, 100, 12, true)[11].contains(" 98s "));
        // 38 samples fit in a 40-column card.
        assert!(card_rows(&app, 40, 12, true)[11].contains(" 38s "));
    }

    #[test]
    fn the_grid_gives_way_before_the_graph_shrinks() {
        let app = app_with(12, Vec::new());
        let shown = |rows: &[String]| rows.iter().filter(|row| row.contains("CPU0")).count();
        let summary_row =
            |rows: &[String]| rows.iter().position(|row| row.contains("Util")).unwrap();

        // Two columns of six rows at 60 columns: 2 borders + 4 graph + 1 + 6.
        let full = card_rows(&app, 60, 13, true);
        assert_eq!(summary_row(&full), 5, "four graph rows first");
        assert!(
            full[11].contains("CPU5") && full[11].contains("CPU11"),
            "{full:#?}"
        );

        // Two grid rows: one row of dense cells and the overflow line.
        let partial = card_rows(&app, 60, 9, true);
        assert_eq!(summary_row(&partial), 5);
        assert_eq!(shown(&partial), 1);
        let labels = partial[6].matches("CPU").count();
        assert!(
            partial[7].contains(&format!("… {} more logical CPUs", 12 - labels)),
            "{partial:#?}"
        );

        let no_grid = card_rows(&app, 60, 7, true);
        assert_eq!(summary_row(&no_grid), 5);
        assert_eq!(shown(&no_grid), 0);

        let one_graph_row = card_rows(&app, 60, 4, true);
        assert_eq!(summary_row(&one_graph_row), 2);

        // Without a graph area the grid follows the summary directly.
        let compact = card_rows(&app, 60, 9, false);
        assert_eq!(summary_row(&compact), 1);
        assert!(compact[2].contains("CPU0"));
    }

    #[test]
    fn the_summary_line_has_a_compact_form() {
        let app = app_with(1, Vec::new());
        let metrics = app.system_metrics();
        assert_eq!(
            summary_line(metrics, 60).to_string(),
            "Util  12%   Load  1m 0.52  5m 0.44  15m 0.29"
        );
        assert_eq!(
            summary_line(metrics, 34).to_string(),
            "Util 12%  Load 0.52 0.44 0.29"
        );
        assert_eq!(summary_line(metrics, 10).to_string().chars().count(), 10);
    }

    #[test]
    fn grid_cells_run_down_the_columns() {
        let cpus = cpus(10);
        let grid = cpu_grid_layout(10, 9, 120, 6);
        assert_eq!(grid.columns, 4);
        let lines = grid_text(&cpus, 120, grid);
        assert_eq!(lines.len(), 3);
        let labels = |line: &str| {
            line.split_whitespace()
                .filter(|word| word.starts_with("CPU"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(&lines[0]), ["CPU0", "CPU3", "CPU6", "CPU9"]);
        assert_eq!(labels(&lines[2]), ["CPU2", "CPU5", "CPU8"]);
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
        let lines = grid_text(&cpus, 120, grid);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].matches(['█', '░']).count() >= 4 * MIN_USEFUL_CPU_GAUGE_WIDTH);
        assert!(lines[2].contains("CPU8"));
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
        let line = &grid_text(&cpus, 44, grid)[0];
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
            cell_text(detailed_cpu_cell(
                &LogicalCpuMetrics {
                    id: crate::linux::LogicalCpuId::for_test(0),
                    utilization_percent: Some(percent),
                },
                24,
                5,
            ))
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
}
