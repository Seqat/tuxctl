use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::App;

use super::{format_uptime, format_usage, hardware, layout};

const SIDE_BY_SIDE_MIN_WIDTH: u16 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OverviewAreas {
    system: Rect,
    hardware: Rect,
}

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let areas = overview_areas(area);
    render_system(frame, app, areas.system);
    hardware::render(frame, app, areas.hardware);
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

    let metrics = app.system_metrics();
    let process_summary = app.process_error().is_none().then(|| app.process_summary());
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
        process_summary,
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
        frame.render_widget(
            Paragraph::new(hardware::usage_bar_line(
                "/  ",
                usage,
                usize::from(inner.width),
            )),
            Rect::new(inner.x, inner.y.saturating_add(text_height), inner.width, 1),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn system_lines(
    hostname: &str,
    kernel: &str,
    uptime: &str,
    process_summary: Option<crate::linux::ProcessSummary>,
    filesystem: &str,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let summary_values = process_summary.map(|summary| {
        (
            summary.total.to_string(),
            summary.running.to_string(),
            summary.zombies.to_string(),
        )
    });
    if height >= 8 {
        let (total, running, zombies) =
            summary_values.unwrap_or_else(|| ("unavailable".into(), "--".into(), "--".into()));
        vec![
            info_line("Host", hostname, width),
            info_line("Kernel", kernel, width),
            info_line("Uptime", uptime, width),
            Line::from(""),
            info_line("Processes", &total, width),
            info_line("Running", &running, width),
            info_line("Zombies", &zombies, width),
            Line::from("Filesystem").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]
    } else if height >= 5 {
        let process_line = process_summary.map_or_else(
            || "Proc  unavailable".to_owned(),
            |summary| {
                format!(
                    "Proc  {}  R {}  Z {}",
                    summary.total, summary.running, summary.zombies
                )
            },
        );
        vec![
            info_line("Host", hostname, width),
            info_line("Kernel", kernel, width),
            info_line("Uptime", uptime, width),
            Line::from(layout::truncate(&process_line, width)),
            Line::from("Filesystem").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]
    } else {
        let process_line = process_summary.map_or_else(
            || "P unavailable".to_owned(),
            |summary| {
                format!(
                    "P {} R {} Z {}",
                    summary.total, summary.running, summary.zombies
                )
            },
        );
        [
            format!("Host {hostname}"),
            format!("Up {uptime}"),
            process_line,
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
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn process(pid: u32, name: &str, state_code: char) -> crate::linux::ProcessInfo {
        crate::linux::ProcessInfo {
            pid,
            name: name.into(),
            cpu_percent: None,
            memory_bytes: 0,
            command: None,
            state: state_code.to_string(),
            parent_pid: 1,
            state_code,
            start_time: u64::from(pid),
            kernel_thread: false,
        }
    }

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

    #[test]
    fn overview_keeps_supported_narrow_viewports_stacked() {
        for width in [40, 46, 80, SIDE_BY_SIDE_MIN_WIDTH - 1] {
            let area = Rect::new(0, 0, width, 57);
            let result = overview_areas(area);
            assert_eq!(result.system.width, width);
            assert_eq!(result.hardware.width, width);
            assert_eq!(result.hardware.y, result.system.bottom());
        }

        let result = overview_areas(Rect::new(0, 0, SIDE_BY_SIDE_MIN_WIDTH, 57));
        assert_eq!(result.system.y, result.hardware.y);
        assert!(result.system.width >= 26);
        assert!(result.hardware.width >= 52);
    }

    #[test]
    fn process_summary_is_unavailable_while_cached_data_is_stale() {
        let mut app = App::default();
        app.update(crate::action::Action::ProcessesUpdated(
            crate::linux::ProcessSnapshot {
                processes: vec![process(1, "running", 'R'), process(2, "zombie", 'Z')],
                error: None,
            },
        ));
        app.update(crate::action::Action::ProcessesUpdated(
            crate::linux::ProcessSnapshot {
                processes: Vec::new(),
                error: Some("proc unavailable".into()),
            },
        ));
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_system(frame, &app, frame.area()))
            .unwrap();

        let stale = rendered_text(&terminal);
        assert!(stale.contains("Processes unavailable"));
        assert!(!stale.contains("Processes 2"));

        app.update(crate::action::Action::ProcessesUpdated(
            crate::linux::ProcessSnapshot {
                processes: vec![process(3, "healthy", 'S')],
                error: None,
            },
        ));
        terminal
            .draw(|frame| render_system(frame, &app, frame.area()))
            .unwrap();
        let healthy = rendered_text(&terminal);
        assert!(!healthy.contains("unavailable"));
        assert!(healthy.contains("Processes 1"));
    }
}
