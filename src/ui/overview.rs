use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::App;

use super::{format_bytes, format_uptime, format_usage, hardware, layout, processes};

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

    let mut used = text_height;
    if let Some(usage) = metrics.root_filesystem.filter(|_| has_gauge) {
        frame.render_widget(
            Paragraph::new(hardware::usage_bar_line(
                "/  ",
                usage,
                usize::from(inner.width),
            )),
            Rect::new(inner.x, inner.y.saturating_add(text_height), inner.width, 1),
        );
        used = used.saturating_add(1);
    }

    let pinned = pinned_lines(app, width, usize::from(inner.height.saturating_sub(used)));
    if !pinned.is_empty() {
        let height = u16::try_from(pinned.len()).unwrap_or(0);
        frame.render_widget(
            Paragraph::new(pinned),
            Rect::new(inner.x, inner.y.saturating_add(used), inner.width, height),
        );
    }
}

/// The pinned processes under Filesystem: a heading and one row each, or
/// nothing when there are no pins or not even one row fits.
fn pinned_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    // A blank line, the heading and at least one process.
    const MIN_HEIGHT: usize = 3;
    const VALUES_WIDTH: usize = 17;
    let pins: Vec<_> = app.pinned_processes().collect();
    if pins.is_empty() || height < MIN_HEIGHT {
        return Vec::new();
    }
    let mut lines = vec![
        Line::from(""),
        Line::from("Pinned").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let rows = height - lines.len();
    let overflow = pins.len() > rows;
    let shown = if overflow { rows - 1 } else { pins.len() };
    let name_width = width.saturating_sub(VALUES_WIDTH).max(1);
    for pin in pins.iter().take(shown) {
        let process = pin.process;
        let name = layout::truncate(&process.name, name_width);
        let line = if pin.exited {
            Line::from(layout::truncate(
                &format!("{name:<name_width$} exited"),
                width,
            ))
            .style(Style::default().fg(Color::DarkGray))
        } else if width > VALUES_WIDTH + 4 {
            Line::from(format!(
                "{name:<name_width$} {:>6} {:>9}",
                processes::format_cpu(process.cpu_percent),
                format_bytes(process.memory_bytes)
            ))
        } else {
            Line::from(layout::truncate(
                &format!("{name} {}", processes::format_cpu(process.cpu_percent)),
                width,
            ))
        };
        lines.push(line);
    }
    if overflow {
        lines.push(Line::from(format!("… {} more", pins.len() - shown)));
    }
    lines
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

    use crate::action::{Action, Tab};
    use crate::linux::{ByteUsage, ProcessIdentity, ProcessSnapshot, SystemMetrics};

    fn snapshot(entries: &[(u32, &str, f64)]) -> Action {
        Action::ProcessesUpdated(ProcessSnapshot {
            processes: entries
                .iter()
                .map(|&(pid, name, cpu)| crate::linux::ProcessInfo {
                    cpu_percent: Some(cpu),
                    memory_bytes: 64 << 20,
                    ..process(pid, name, 'S')
                })
                .collect(),
            error: None,
        })
    }

    /// Pins `pids` on the Processes tab, then returns to Overview.
    fn overview_with_pins(entries: &[(u32, &str, f64)], pids: &[u32]) -> App {
        let mut app = App::default();
        app.update(Action::SystemMetricsUpdated(SystemMetrics {
            root_filesystem: Some(ByteUsage {
                used: 50 << 30,
                total: 100 << 30,
            }),
            ..SystemMetrics::default()
        }));
        app.update(Action::SelectTab(Tab::Processes));
        app.update(snapshot(entries));
        for &pid in pids {
            app.update(Action::SelectProcess(ProcessIdentity {
                pid,
                start_time: u64::from(pid),
            }));
            app.update(Action::TogglePin);
        }
        app.update(Action::SelectTab(Tab::Overview));
        app
    }

    fn system_panel(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_system(frame, app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn pinned_processes_are_listed_under_the_filesystem() {
        let app = overview_with_pins(
            &[
                (1, "postgres", 12.5),
                (2, "firefox", 30.0),
                (3, "bash", 0.0),
            ],
            &[2, 1],
        );
        let rows = system_panel(&app, 36, 20);
        let at = |text: &str| rows.iter().position(|row| row.contains(text));

        let pinned = at("Pinned").expect("section heading");
        assert!(at("Filesystem").unwrap() < pinned);
        assert!(rows[pinned + 1].contains("firefox"), "{rows:#?}");
        assert!(rows[pinned + 1].contains("30.0%"), "{rows:#?}");
        assert!(rows[pinned + 1].contains("64.0 MiB"), "{rows:#?}");
        assert!(
            rows[pinned + 2].contains("postgres"),
            "pin order: {rows:#?}"
        );
        assert!(at("bash").is_none(), "unpinned processes are not listed");
    }

    #[test]
    fn exited_pins_are_shown_dimmed_on_overview() {
        let mut app = overview_with_pins(&[(1, "worker", 5.0), (2, "bash", 1.0)], &[1]);
        app.update(snapshot(&[(2, "bash", 1.0)]));

        let rows = system_panel(&app, 36, 20);
        let row = rows.iter().find(|row| row.contains("worker")).unwrap();
        assert!(row.contains("exited"), "{row}");

        let mut terminal = Terminal::new(TestBackend::new(36, 20)).unwrap();
        terminal
            .draw(|frame| render_system(frame, &app, frame.area()))
            .unwrap();
        let y = rows.iter().position(|row| row.contains("worker")).unwrap() as u16;
        assert_eq!(terminal.backend().buffer()[(2, y)].fg, Color::DarkGray);
    }

    #[test]
    fn the_pinned_section_needs_pins_and_room() {
        let none = overview_with_pins(&[(1, "a", 1.0)], &[]);
        assert!(!system_panel(&none, 36, 20)
            .iter()
            .any(|row| row.contains("Pinned")));

        let pinned = overview_with_pins(&[(1, "a", 1.0)], &[1]);
        for height in [5, 8, 10] {
            let rows = system_panel(&pinned, 40, height);
            if let Some(heading) = rows.iter().position(|row| row.contains("Pinned")) {
                assert!(heading + 1 < rows.len(), "no heading without a row");
            }
        }
    }

    #[test]
    fn pins_that_do_not_fit_are_counted() {
        let entries: Vec<(u32, String, f64)> = (1..=8)
            .map(|pid| (pid, format!("proc{pid}"), 1.0))
            .collect();
        let entries: Vec<(u32, &str, f64)> = entries
            .iter()
            .map(|(pid, name, cpu)| (*pid, name.as_str(), *cpu))
            .collect();
        let app = overview_with_pins(&entries, &[1, 2, 3, 4, 5, 6, 7, 8]);

        // 16 rows leave room for the heading and a few of the 8 pins.
        let rows = system_panel(&app, 36, 16);
        assert!(
            rows.iter()
                .any(|row| row.contains("… ") && row.contains("more")),
            "{rows:#?}"
        );
    }
}
