use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::{
    action::{MouseTarget, ProcessSort, ProcessSortField, SignalConfirmButton},
    app::calculate_scroll,
    app::{App, ProcessSignalConfirmation},
    linux::{ProcessIdentity, ProcessInfo, ProcessSignal, SystemMetrics},
};

use super::{format_bytes, layout};

pub struct ProcessRender {
    pub rows: Vec<(ProcessIdentity, Rect)>,
    pub headers: Vec<(ProcessSortField, Rect)>,
    pub scroll_area: Rect,
    pub start: usize,
    pub height: usize,
}

const COLUMN_SPACING: u16 = 1;
const COLUMN_WIDTHS: [Constraint; 4] = [
    Constraint::Length(10),
    Constraint::Percentage(45),
    Constraint::Length(9),
    Constraint::Min(10),
];

pub fn render(frame: &mut Frame, app: &App, area: Rect) -> ProcessRender {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    render_resource_summary(frame, app, sections[0]);
    render_controls(frame, app, sections[1]);

    let table_area = sections[2];
    let row_area = Rect::new(
        table_area.x,
        table_area.y.saturating_add(table_area.height.min(1)),
        table_area.width,
        table_area.height.saturating_sub(1),
    );
    let height = usize::from(row_area.height);
    let start = calculate_scroll(
        app.process_count(),
        app.selected_process_index(),
        app.process_scroll(),
        height,
    );
    let end = start.saturating_add(height).min(app.process_count());

    let selected = app.selected_process().map(ProcessInfo::identity);
    let hovered = app.hovered();
    let mut hit_rows = Vec::with_capacity(end.saturating_sub(start));
    let rows = (start..end).filter_map(|index| {
        let process = app.process_at(index)?;
        let offset = u16::try_from(index.saturating_sub(start)).ok()?;
        hit_rows.push((
            process.identity(),
            Rect::new(
                row_area.x,
                row_area.y.saturating_add(offset),
                row_area.width,
                1,
            ),
        ));

        let style = if selected == Some(process.identity()) {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else if hovered == Some(&MouseTarget::ProcessRow(process.identity())) {
            Style::default().bg(Color::Rgb(35, 35, 35))
        } else {
            Style::default()
        };
        Some(
            Row::new([
                Cell::from(format!(
                    "{}{}",
                    if selected == Some(process.identity()) {
                        "> "
                    } else {
                        "  "
                    },
                    process.pid
                )),
                Cell::from(process.name.as_str()),
                Cell::from(format_cpu(process.cpu_percent)),
                Cell::from(format_bytes(process.memory_bytes)),
            ])
            .style(style),
        )
    });

    let sort = app.process_sort();
    let header = Row::new([
        sort_header("PID", ProcessSortField::Pid, sort, hovered),
        sort_header("NAME", ProcessSortField::Name, sort, hovered),
        sort_header("CPU", ProcessSortField::Cpu, sort, hovered),
        sort_header("MEMORY", ProcessSortField::Memory, sort, hovered),
    ]);
    let table = Table::new(rows, COLUMN_WIDTHS)
        .header(header)
        .column_spacing(COLUMN_SPACING)
        .flex(Flex::Start);
    frame.render_widget(table, table_area);

    let header_area = Rect::new(
        table_area.x,
        table_area.y,
        table_area.width,
        table_area.height.min(1),
    );
    let header_columns = Layout::horizontal(COLUMN_WIDTHS)
        .flex(Flex::Start)
        .spacing(COLUMN_SPACING)
        .split(header_area);
    let headers = [
        ProcessSortField::Pid,
        ProcessSortField::Name,
        ProcessSortField::Cpu,
        ProcessSortField::Memory,
    ]
    .into_iter()
    .zip(header_columns.iter().copied())
    .filter(|(_, area)| !area.is_empty())
    .collect();

    if app.process_count() == 0 && row_area.height > 0 {
        let message = if app.process_error().is_some() {
            "Process data unavailable"
        } else if app.process_search_query().is_empty() {
            "No processes found"
        } else {
            "No matching processes"
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            row_area,
        );
    }

    ProcessRender {
        rows: hit_rows,
        headers,
        scroll_area: row_area,
        start,
        height,
    }
}

fn sort_header(
    label: &'static str,
    field: ProcessSortField,
    sort: ProcessSort,
    hovered: Option<&MouseTarget>,
) -> Cell<'static> {
    let active = sort.field == field;
    let label = if active {
        format!("{label} {}", if sort.descending { '▼' } else { '▲' })
    } else {
        label.into()
    };
    let style = if active {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if hovered == Some(&MouseTarget::ProcessSortHeader(field)) {
        Style::default().fg(Color::Cyan).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Cyan)
    };

    Cell::from(label).style(style)
}

fn render_resource_summary(frame: &mut Frame, app: &App, area: Rect) {
    let text = resource_summary_text(
        app.process_count(),
        app.system_metrics(),
        usize::from(area.width),
    );
    frame.render_widget(Paragraph::new(text), area);
}

fn resource_summary_text(count: usize, metrics: &SystemMetrics, width: usize) -> String {
    let cpu = metrics.cpu_percent;
    let ram = metrics.memory.map(|memory| memory.percent());
    let values = metrics.memory.map(|memory| {
        format!(
            "{} / {}",
            format_bytes(memory.used),
            format_bytes(memory.total)
        )
    });
    let count_long = format!("{count} processes");
    let count_short = format!("{count} proc");
    let candidates = [
        resource_summary_candidate(&count_long, cpu, ram, values.as_deref(), Some(10)),
        resource_summary_candidate(&count_long, cpu, ram, values.as_deref(), Some(6)),
        resource_summary_candidate(&count_long, cpu, ram, values.as_deref(), None),
        resource_summary_candidate(&count_long, cpu, ram, None, Some(6)),
        resource_summary_candidate(&count_long, cpu, ram, None, None),
        resource_summary_candidate(&count_short, cpu, ram, None, None),
    ];
    let summary = candidates
        .into_iter()
        .find(|candidate| candidate.chars().count().saturating_add(1) <= width)
        .unwrap_or(count_short);

    layout::truncate(&format!(" {summary}"), width)
}

fn resource_summary_candidate(
    count: &str,
    cpu: Option<f64>,
    ram: Option<f64>,
    values: Option<&str>,
    gauge_width: Option<usize>,
) -> String {
    let cpu = resource_metric("CPU", cpu, gauge_width);
    let ram = resource_metric("RAM", ram, gauge_width);
    let values = values.map_or_else(String::new, |values| format!("  {values}"));
    format!("{count}   {cpu}   {ram}{values}")
}

fn resource_metric(label: &str, percent: Option<f64>, gauge_width: Option<usize>) -> String {
    let percent_text = system_percent(percent);
    gauge_width.map_or_else(
        || format!("{label} {percent_text}"),
        |width| {
            format!(
                "{label} {percent_text} [{}]",
                utilization_bar(percent, width)
            )
        },
    )
}

fn utilization_bar(percent: Option<f64>, width: usize) -> String {
    let filled = percent
        .map(|percent| ((percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize)
        .unwrap_or(0)
        .min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn system_percent(percent: Option<f64>) -> String {
    percent
        .map(|percent| format!("{:>4}", format!("{:.0}%", percent.clamp(0.0, 100.0))))
        .unwrap_or_else(|| " N/A".into())
}

fn render_controls(frame: &mut Frame, app: &App, area: Rect) {
    let text = if app.process_searching() {
        format!(" Search: {}_", app.process_search_query())
    } else if let Some(msg) = app.process_action_message() {
        format!(" {msg}")
    } else if let Some(error) = app.process_error() {
        format!(" Refresh error: {error}")
    } else if app.process_search_query().is_empty() {
        let sort_info = format!(
            "Sort: {}{}",
            app.process_sort().field.as_str(),
            if app.process_sort().descending {
                "▼"
            } else {
                "▲"
            }
        );
        if area.width >= 85 {
            format!(" {sort_info}   / search   Enter details   t term   K kill")
        } else if area.width >= 60 {
            " / search   Enter details   t term   K kill".into()
        } else {
            " / find   Enter view".into()
        }
    } else if area.width >= 75 {
        format!(
            " Filter: \"{}\"   / edit   Esc clear   t term   K kill",
            app.process_search_query()
        )
    } else {
        format!(" Filter: \"{}\"   Esc clear", app.process_search_query())
    };
    frame.render_widget(Paragraph::new(text), area);
}

pub fn render_signal_confirmation(
    frame: &mut Frame,
    confirmation: &ProcessSignalConfirmation,
    hovered: Option<&MouseTarget>,
    area: Rect,
) -> (Rect, Rect) {
    let popup = layout::centered_rect(area, 60, 9);
    if popup.width < 20 || popup.height < 6 {
        return (Rect::default(), Rect::default());
    }

    let (title, prompt, border_style, confirm_label, confirm_base_style, confirm_focused_style) =
        match confirmation.signal {
            ProcessSignal::Term => (
                " Terminate Process (SIGTERM) ",
                "Terminate this process gracefully?",
                Style::default().fg(Color::Yellow),
                " [ Terminate ] ",
                Style::default().fg(Color::Yellow),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            ProcessSignal::Kill => (
                " Force Kill Process (SIGKILL) ",
                "FORCE KILL this process immediately? (Uncatchable)",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                " [ Force Kill ] ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let text_area = Rect::new(inner.x, inner.y, inner.width, inner.height.min(3));
    let text_lines = vec![
        Line::from(prompt).style(border_style),
        Line::from(format!(
            "Process: {} (PID: {})",
            confirmation.name, confirmation.identity.pid
        )),
        Line::from(format!("Signal:  {}", confirmation.signal.as_str())),
    ];
    frame.render_widget(Paragraph::new(text_lines), text_area);

    let cancel_label = " [ Cancel ] ";
    let cancel_width = cancel_label.len() as u16;
    let confirm_width = confirm_label.len() as u16;
    let gap = 4;
    let total_buttons_width = cancel_width
        .saturating_add(gap)
        .saturating_add(confirm_width);

    let button_y = inner.y.saturating_add(inner.height.saturating_sub(3));

    let (cancel_rect, confirm_rect) = if inner.width >= total_buttons_width {
        let start_x = inner
            .x
            .saturating_add(inner.width.saturating_sub(total_buttons_width) / 2);
        (
            Rect::new(start_x, button_y, cancel_width, 1),
            Rect::new(
                start_x.saturating_add(cancel_width).saturating_add(gap),
                button_y,
                confirm_width,
                1,
            ),
        )
    } else {
        let half = inner.width / 2;
        (
            Rect::new(inner.x, button_y, half.saturating_sub(1), 1),
            Rect::new(
                inner.x.saturating_add(half),
                button_y,
                inner.width.saturating_sub(half),
                1,
            ),
        )
    };

    let cancel_style = if confirmation.focused_button == SignalConfirmButton::Cancel {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if hovered == Some(&MouseTarget::ProcessSignalCancel) {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    let confirm_style = if confirmation.focused_button == SignalConfirmButton::Confirm {
        confirm_focused_style
    } else if hovered == Some(&MouseTarget::ProcessSignalConfirm) {
        confirm_base_style.bg(Color::DarkGray)
    } else {
        confirm_base_style
    };

    frame.render_widget(
        Paragraph::new(cancel_label)
            .style(cancel_style)
            .alignment(ratatui::layout::Alignment::Center),
        cancel_rect,
    );
    frame.render_widget(
        Paragraph::new(confirm_label)
            .style(confirm_style)
            .alignment(ratatui::layout::Alignment::Center),
        confirm_rect,
    );

    if inner.height >= 5 {
        let hint_area = Rect::new(
            inner.x,
            inner.y.saturating_add(inner.height.saturating_sub(1)),
            inner.width,
            1,
        );
        frame.render_widget(
            Paragraph::new("Tab / ← → focus   Enter select   Esc cancel")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            hint_area,
        );
    }

    (cancel_rect, confirm_rect)
}

pub fn render_detail(frame: &mut Frame, process: Option<&ProcessInfo>, area: Rect) {
    let popup = layout::centered_rect(area, 72, 12);
    if popup.width == 0 || popup.height == 0 {
        return;
    }

    let lines = process.map_or_else(
        || vec![Line::from("Process is no longer available")],
        |process| {
            vec![
                Line::from(format!("PID:         {}", process.pid)),
                Line::from(format!("Name:        {}", process.name)),
                Line::from(format!(
                    "Command:     {}",
                    process.command.as_deref().unwrap_or("N/A")
                )),
                Line::from(format!("CPU:         {}", format_cpu(process.cpu_percent))),
                Line::from(format!(
                    "Memory:      {}",
                    format_bytes(process.memory_bytes)
                )),
                Line::from(format!("State:       {}", process.state)),
                Line::from(format!("Parent PID:  {}", process.parent_pid)),
                Line::from(""),
                Line::from("Read-only inspection; Esc closes")
                    .style(Style::default().fg(Color::DarkGray)),
            ]
        },
    );

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Process Details "),
        ),
        popup,
    );
}

fn format_cpu(percent: Option<f64>) -> String {
    percent
        .map(|percent| format!("{percent:.1}%"))
        .unwrap_or_else(|| "N/A".into())
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{action::Action, linux::ByteUsage};

    use super::*;

    fn metrics() -> SystemMetrics {
        SystemMetrics {
            cpu_percent: Some(14.0),
            memory: Some(ByteUsage {
                used: 8 * 1024 * 1024 * 1024,
                total: 32 * 1024 * 1024 * 1024,
            }),
            ..SystemMetrics::default()
        }
    }

    #[test]
    fn wide_resource_summary_includes_values_and_full_gauges() {
        let text = resource_summary_text(393, &metrics(), 120);

        assert!(text.contains("393 processes"));
        assert!(text.contains("CPU  14%"));
        assert!(text.contains("RAM  25%"));
        assert!(text.contains("8.0 GiB / 32.0 GiB"));
        assert_eq!(text.matches(['█', '░']).count(), 20);
        assert!(text.chars().count() <= 120);
    }

    #[test]
    fn medium_resource_summary_preserves_values_before_gauges() {
        let text = resource_summary_text(393, &metrics(), 70);

        assert!(text.contains("CPU  14%"));
        assert!(text.contains("RAM  25%"));
        assert!(text.contains("8.0 GiB / 32.0 GiB"));
        assert!(!text.contains(['█', '░']));
        assert!(text.chars().count() <= 70);
    }

    #[test]
    fn compact_resource_summary_uses_short_gauges_when_they_fit() {
        let text = resource_summary_text(393, &metrics(), 54);

        assert!(text.contains("CPU  14%"));
        assert!(text.contains("RAM  25%"));
        assert_eq!(text.matches(['█', '░']).count(), 12);
        assert!(!text.contains("GiB"));
        assert!(text.chars().count() <= 54);
    }

    #[test]
    fn narrow_resource_summary_keeps_count_and_percentages() {
        let text = resource_summary_text(393, &metrics(), 40);

        assert!(text.contains("393 processes"));
        assert!(text.contains("CPU  14%"));
        assert!(text.contains("RAM  25%"));
        assert!(!text.contains(['█', '░']));
        assert!(!text.contains("GiB"));
        assert!(text.chars().count() <= 40);
    }

    #[test]
    fn processes_render_uses_cached_metrics_without_overlapping_table_geometry() {
        let mut app = App::default();
        app.update(Action::SelectTab(crate::action::Tab::Processes));
        app.update(Action::SystemMetricsUpdated(metrics()));
        let backend = TestBackend::new(40, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut rendered = None;
        terminal
            .draw(|frame| rendered = Some(render(frame, &app, frame.area())))
            .unwrap();
        let rendered = rendered.unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(text.contains("CPU  14%"));
        assert!(text.contains("RAM  25%"));
        assert!(rendered.headers.iter().all(|(_, area)| area.y == 2));
        assert_eq!(rendered.scroll_area.y, 3);
        assert_eq!(rendered.scroll_area.height, 12);
    }
}
