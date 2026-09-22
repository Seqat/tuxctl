use ratatui::{layout::Rect, text::Line, widgets::Paragraph, Frame};

use crate::{
    app::App,
    linux::{HardwareInventory, LogicalCpuMetrics, SystemMetrics},
};

use super::{
    hardware::{section_heading, utilization_bar},
    layout,
};

const PREFERRED_CPU_CELL_WIDTH: usize = 20;
const MIN_DETAILED_CPU_CELL_WIDTH: usize = 16;
const MAX_DETAILED_CPU_COLUMNS: usize = 4;
const MIN_USEFUL_CPU_GAUGE_WIDTH: usize = 6;
const CPU_CELL_GAP: usize = 2;
/// Dense cells already separate label and value, so one space between cells suffices.
const DENSE_CPU_CELL_GAP: usize = 1;
/// Heading, model, utilization history and load average.
const SUMMARY_LINES: usize = 4;
/// Grid rows the CPU section claims before lower sections get their minimum.
const PRIORITY_GRID_ROWS: usize = 4;

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

/// Height the CPU section claims first: its summary plus a few rows of the
/// densest grid, with an overflow line when that cannot show every CPU.
pub(super) fn priority_height(metrics: &SystemMetrics, width: usize) -> u16 {
    let count = metrics.logical_cpus.len();
    if count == 0 {
        return SUMMARY_LINES as u16;
    }
    let densest_columns = grid_layout(metrics, width, 1).columns.max(1);
    let rows_for_all = count.div_ceil(densest_columns);
    let rows = rows_for_all.min(PRIORITY_GRID_ROWS);
    let height = SUMMARY_LINES + rows + usize::from(rows_for_all > rows);
    u16::try_from(height).unwrap_or(u16::MAX)
}

/// Height at which every CPU fits in the preferred grid with breathing room.
pub(super) fn comfortable_height(metrics: &SystemMetrics, width: usize) -> u16 {
    let grid = grid_layout(metrics, width, metrics.logical_cpus.len());
    u16::try_from(SUMMARY_LINES + 2 + grid.rows).unwrap_or(u16::MAX)
}

/// The model line is dropped in a minimal section so the overflow line still fits.
fn shows_model_line(metrics: &SystemMetrics, height: usize) -> bool {
    height > SUMMARY_LINES || metrics.logical_cpus.is_empty()
}

/// Chooses the grid from the height the section actually received, reserving a
/// row for the overflow line whenever not every CPU fits.
pub(super) fn grid_for_height(
    metrics: &SystemMetrics,
    width: usize,
    height: usize,
) -> CpuGridLayout {
    let header = SUMMARY_LINES - usize::from(!shows_model_line(metrics, height));
    let rows = height.saturating_sub(header);
    let grid = grid_layout(metrics, width, rows);
    if grid.visible < metrics.logical_cpus.len() {
        grid_layout(metrics, width, rows.saturating_sub(1))
    } else {
        grid
    }
}

pub(super) fn render(
    frame: &mut Frame,
    app: &App,
    inventory: Option<&HardwareInventory>,
    metrics: &SystemMetrics,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let grid = grid_for_height(metrics, width, height);
    let show_model = shows_model_line(metrics, height);
    let grid_lines = cpu_grid_lines(&metrics.logical_cpus, width, grid);
    let has_overflow = grid.visible < metrics.logical_cpus.len();
    let content_height = (SUMMARY_LINES - usize::from(!show_model))
        .saturating_add(grid_lines.len())
        .saturating_add(usize::from(has_overflow));
    let spacing = height.saturating_sub(content_height).min(2);
    let mut lines = vec![section_heading("CPU")];

    if show_model && lines.len() < height {
        let model = match inventory {
            None => "Discovering hardware…",
            Some(inventory) => inventory
                .cpus
                .first()
                .map(|cpu| cpu.model.as_str())
                .unwrap_or("Unavailable / none detected"),
        };
        lines.push(Line::from(layout::truncate(model, width)));
    }
    if spacing >= 1 && lines.len() < height {
        lines.push(Line::from(""));
    }
    if lines.len() < height {
        let percent = format_percent(metrics.cpu_percent);
        let prefix = format!("Util  {percent:>4}  ");
        lines.push(Line::from(layout::truncate(
            &format!(
                "{prefix}{}",
                utilization_history(app, width.saturating_sub(prefix.chars().count()))
            ),
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

/// Rows the section actually draws at `height`: content plus at most two
/// spacer lines. Rows beyond that are better used as gaps between sections.
pub(super) fn fitted_height(metrics: &SystemMetrics, width: usize, height: u16) -> u16 {
    let grid = grid_for_height(metrics, width, usize::from(height));
    let header = SUMMARY_LINES - usize::from(!shows_model_line(metrics, usize::from(height)));
    let content = header + grid.rows + usize::from(grid.visible < metrics.logical_cpus.len());
    height.min(u16::try_from(content + 2).unwrap_or(u16::MAX))
}

fn cpu_grid_lines(cpus: &[LogicalCpuMetrics], width: usize, grid: CpuGridLayout) -> Vec<String> {
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
    let gap = " ".repeat(gap_width);
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
                        dense_cpu_cell(cpu, cell_width, label_width)
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

fn dense_cpu_cell(cpu: &LogicalCpuMetrics, cell_width: usize, label_width: usize) -> String {
    let label = format!("CPU{}", cpu.id.index());
    let percent = format_percent(cpu.utilization_percent);
    if cell_width < dense_cell_width(label_width) {
        // No room for the level glyph; keep the separator and the value.
        return format!("{label:<label_width$} {percent:>4}");
    }
    format!(
        "{label:<label_width$} {}{percent:>4}",
        utilization_level(cpu.utilization_percent)
    )
}

/// The CPU sparkline followed by the time span it covers, e.g. `▂▃▅  60s`.
/// The span counts only the samples that fit, so it shrinks on narrow panels.
fn utilization_history(app: &App, width: usize) -> String {
    const LABEL_GAP: &str = "  ";
    const MIN_SPARKLINE_WIDTH: usize = 4;
    let history = app.aggregate_cpu_history();
    let interval = app.cpu_history_interval();
    let widest_label = format_window(interval.saturating_mul(history.capacity() as u32));
    let sparkline_width = width
        .saturating_sub(LABEL_GAP.len() + widest_label.chars().count())
        .min(history.capacity());
    if sparkline_width < MIN_SPARKLINE_WIDTH {
        return history_sparkline(history.iter(), width);
    }
    let sparkline = history_sparkline(history.iter(), sparkline_width);
    let window = format_window(interval.saturating_mul(sparkline_width as u32));
    format!(
        "{}{LABEL_GAP}{window}",
        pad_cell(&sparkline, sparkline_width)
    )
}

/// Compact span: `15s`, `90s`, `2m`, `2m30s`, `1h`; sub-second parts as `4.8s`.
fn format_window(window: std::time::Duration) -> String {
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

    fn util_line(interval_secs: u64, width: u16) -> String {
        let app = App::default().with_collector_periods(
            crate::app::CollectorPeriods::for_sampling_interval(std::time::Duration::from_secs(
                interval_secs,
            )),
        );
        let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, None, app.system_metrics(), frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .find(|row| row.starts_with("Util"))
            .expect("utilization line")
    }

    #[test]
    fn history_label_states_the_window_it_covers() {
        assert!(util_line(1, 100).trim_end().ends_with("  60s"));
        assert!(util_line(5, 100).trim_end().ends_with("  5m"));
        // Only 23 samples fit next to the label at 40 columns.
        assert!(
            util_line(1, 40).trim_end().ends_with("  23s"),
            "{}",
            util_line(1, 40)
        );
    }

    #[test]
    fn history_label_is_dropped_when_there_is_no_room() {
        let line = util_line(1, 16);
        assert!(!line.contains('s'), "{line}");
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
                render(frame, &app, None, app.system_metrics(), frame.area());
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
    fn cpu_model_shows_unavailable_when_inventory_has_no_cpus() {
        let app = App::default();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let inventory = crate::linux::HardwareInventory::default();
        let metrics = SystemMetrics::default();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &app,
                    Some(&inventory),
                    &metrics,
                    Rect::new(0, 0, 80, 20),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let content = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("Unavailable / none detected"));
    }
}
