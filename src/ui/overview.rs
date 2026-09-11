use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame,
};

use crate::app::App;

use super::{format_uptime, format_usage, hardware, layout};

const SIDE_BY_SIDE_MIN_WIDTH: u16 = 58;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OverviewAreas {
    system: Rect,
    hardware: Rect,
}

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    render_status(frame, sections[0]);

    let areas = overview_areas(sections[1]);
    render_system(frame, app, areas.system);
    hardware::render(frame, app, areas.hardware);
}

fn render_status(frame: &mut Frame, area: Rect) {
    let text = if area.width >= 75 {
        " System & Hardware Dashboard"
    } else if area.width >= 54 {
        " System & Hardware Dashboard   ? Help"
    } else if area.width >= 28 {
        " System + Hardware   ? Help"
    } else {
        " Overview   ?"
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Cyan)),
        area,
    );
}

fn overview_areas(area: Rect) -> OverviewAreas {
    if area.width >= SIDE_BY_SIDE_MIN_WIDTH {
        let system_width = (area.width / 3).clamp(26, 38).min(area.width);
        let columns =
            Layout::horizontal([Constraint::Length(system_width), Constraint::Min(0)]).split(area);
        OverviewAreas {
            system: columns[0],
            hardware: columns[1],
        }
    } else {
        let system_height = if area.height >= 22 {
            11
        } else if area.height >= 12 {
            area.height / 2
        } else {
            area.height.min(6)
        };
        let rows =
            Layout::vertical([Constraint::Length(system_height), Constraint::Min(0)]).split(area);
        OverviewAreas {
            system: rows[0],
            hardware: rows[1],
        }
    }
}

fn render_system(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let block = Block::default().borders(Borders::ALL).title(" System ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let metrics = app.overview();
    let summary = app.process_summary();
    let hostname = metrics.system_identity.hostname.as_deref().unwrap_or("N/A");
    let kernel = metrics
        .system_identity
        .kernel_release
        .as_deref()
        .unwrap_or("N/A");
    let uptime = metrics
        .uptime
        .map(format_uptime)
        .unwrap_or_else(|| "N/A".into());
    let filesystem = metrics
        .root_filesystem
        .map(format_usage)
        .unwrap_or_else(|| "N/A".into());

    let has_gauge = metrics.root_filesystem.is_some() && inner.height >= 2;
    let available_text_height = inner.height.saturating_sub(u16::from(has_gauge));
    let width = usize::from(inner.width);
    let mut lines = system_lines(
        hostname,
        kernel,
        &uptime,
        summary.total,
        summary.running,
        summary.zombies,
        &filesystem,
        width,
        usize::from(available_text_height),
    );
    lines.truncate(usize::from(available_text_height));
    let text_height = u16::try_from(lines.len()).unwrap_or(available_text_height);
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(inner.x, inner.y, inner.width, text_height),
    );

    if let Some(usage) = metrics.root_filesystem.filter(|_| has_gauge) {
        let percent = usage.percent();
        let label = layout::truncate(
            &format!("/  {filesystem}  {percent:.0}%"),
            usize::from(inner.width),
        );
        frame.render_widget(
            Gauge::default()
                .gauge_style(Style::default().fg(Color::Cyan))
                .ratio((percent / 100.0).clamp(0.0, 1.0))
                .label(label),
            Rect::new(inner.x, inner.y.saturating_add(text_height), inner.width, 1),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn system_lines(
    hostname: &str,
    kernel: &str,
    uptime: &str,
    total: usize,
    running: usize,
    zombies: usize,
    filesystem: &str,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if height >= 8 {
        vec![
            info_line("Host", hostname, width),
            info_line("Kernel", kernel, width),
            info_line("Uptime", uptime, width),
            Line::from(""),
            info_line("Processes", &total.to_string(), width),
            info_line("Running", &running.to_string(), width),
            info_line("Zombies", &zombies.to_string(), width),
            Line::from("Filesystem").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]
    } else if height >= 5 {
        vec![
            info_line("Host", hostname, width),
            info_line("Kernel", kernel, width),
            info_line("Uptime", uptime, width),
            Line::from(layout::truncate(
                &format!("Proc  {total}  R {running}  Z {zombies}"),
                width,
            )),
            Line::from("Filesystem").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]
    } else {
        [
            format!("Host {hostname}"),
            format!("Up {uptime}"),
            format!("P {total} R {running} Z {zombies}"),
            format!("/ {filesystem}"),
        ]
        .into_iter()
        .take(height)
        .map(|line| Line::from(layout::truncate(&line, width)))
        .collect()
    }
}

fn info_line(label: &str, value: &str, width: usize) -> Line<'static> {
    const LABEL_WIDTH: usize = 10;
    if width <= LABEL_WIDTH {
        return Line::from(layout::truncate(&format!("{label} {value}"), width));
    }

    let value = layout::truncate(value, width.saturating_sub(LABEL_WIDTH));
    Line::from(format!("{label:<LABEL_WIDTH$}{value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_uses_side_by_side_areas_when_width_allows() {
        let area = Rect::new(3, 4, 100, 20);
        let result = overview_areas(area);

        assert_eq!(result.system, Rect::new(3, 4, 33, 20));
        assert_eq!(result.hardware, Rect::new(36, 4, 67, 20));
    }

    #[test]
    fn overview_stacks_and_clamps_areas_at_narrow_or_tiny_sizes() {
        let narrow = overview_areas(Rect::new(2, 3, 40, 18));
        assert_eq!(narrow.system, Rect::new(2, 3, 40, 9));
        assert_eq!(narrow.hardware, Rect::new(2, 12, 40, 9));

        let tiny = overview_areas(Rect::new(u16::MAX - 1, u16::MAX - 1, 1, 1));
        assert_eq!(tiny.system.width, 1);
        assert_eq!(tiny.system.height, 1);
        assert_eq!(tiny.hardware.height, 0);
    }
}
