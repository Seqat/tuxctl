use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};

use crate::{
    action::MouseTarget,
    app::App,
    linux::{priority_label, JournalEntry},
};

use super::layout;

pub struct LogRender {
    pub rows: Vec<(u64, Rect)>,
    pub scroll_area: Rect,
    pub start: usize,
    pub height: usize,
}

const COLUMN_SPACING: u16 = 1;
const COLUMN_WIDTHS: [Constraint; 4] = [
    Constraint::Length(11),
    Constraint::Percentage(28),
    Constraint::Length(7),
    Constraint::Min(18),
];

pub fn render(frame: &mut Frame, app: &App, area: Rect) -> LogRender {
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    render_status(frame, app, sections[0]);

    let table_area = sections[1];
    let row_area = Rect::new(
        table_area.x,
        table_area.y.saturating_add(table_area.height.min(1)),
        table_area.width,
        table_area.height.saturating_sub(1),
    );
    let height = usize::from(row_area.height);
    let start = crate::app::calculate_scroll(
        app.log_count(),
        app.selected_log_index(),
        app.log_scroll(),
        height,
    );
    let end = start.saturating_add(height).min(app.log_count());
    let selected = app.selected_log().map(|entry| entry.id);
    let hovered = app.hovered();
    let mut hit_rows = Vec::with_capacity(end.saturating_sub(start));

    let rows = (start..end).filter_map(|index| {
        let entry = app.log_at(index)?;
        let offset = u16::try_from(index.saturating_sub(start)).ok()?;
        hit_rows.push((
            entry.id,
            Rect::new(
                row_area.x,
                row_area.y.saturating_add(offset),
                row_area.width,
                1,
            ),
        ));

        let prefix = if selected == Some(entry.id) {
            "> "
        } else {
            "  "
        };

        let style = if selected == Some(entry.id) {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else if hovered == Some(&MouseTarget::LogRow(entry.id)) {
            Style::default().bg(Color::Rgb(35, 35, 35))
        } else {
            Style::default()
        };

        Some(
            Row::new([
                Cell::from(format!(
                    "{prefix}{}",
                    format_timestamp(entry.timestamp_micros)
                )),
                Cell::from(entry.source.as_str()),
                Cell::from(priority_label(entry.priority)).style(priority_style(entry.priority)),
                Cell::from(single_line(&entry.message)),
            ])
            .style(style),
        )
    });

    let header = Row::new(["  TIME UTC", "UNIT/SOURCE", "PRIORITY", "MESSAGE"]).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(
        Table::new(rows, COLUMN_WIDTHS)
            .header(header)
            .column_spacing(COLUMN_SPACING)
            .flex(Flex::Start),
        table_area,
    );

    if app.log_count() == 0 && row_area.height > 0 {
        let message = if app.log_error().is_some() {
            "Journal unavailable"
        } else if app.log_search_query().is_empty() {
            "Waiting for journal entries…"
        } else {
            "No matching journal entries"
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            row_area,
        );
    }

    LogRender {
        rows: hit_rows,
        scroll_area: row_area,
        start,
        height,
    }
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let (mode_badge, mode_style) = if app.log_paused() {
        (
            "[PAUSED]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if app.log_following() {
        (
            "[FOLLOW]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("[MANUAL]", Style::default().fg(Color::Cyan))
    };

    let pause_hint = if app.log_paused() {
        "Space resume"
    } else {
        "Space pause"
    };

    let mut spans = vec![
        Span::raw(" "),
        Span::styled(mode_badge, mode_style),
        Span::raw(" "),
    ];

    if app.log_searching() {
        spans.push(Span::styled(
            format!("Search: {}_", app.log_search_query()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.log_search_query().is_empty() {
        if area.width >= 55 {
            spans.push(Span::raw(format!(
                "{} entries   / search   f follow   {pause_hint}   Enter details",
                app.log_count()
            )));
        } else {
            spans.push(Span::raw(format!("{} entries", app.log_count())));
        }
    } else {
        spans.push(Span::raw(format!(
            "Filter: \"{}\" ({} matches; Esc clear)",
            app.log_search_query(),
            app.log_count()
        )));
    }

    if app.log_dropped() > 0 {
        spans.push(Span::styled(
            format!("   {} dropped", app.log_dropped()),
            Style::default().fg(Color::Yellow),
        ));
    }

    if let Some(error) = app.log_error() {
        spans.push(Span::styled(
            format!("   Error: {error}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn render_detail(frame: &mut Frame, entry: Option<&JournalEntry>, area: Rect) {
    let popup = layout::centered_rect(area, 82, 14);
    if popup.width == 0 || popup.height == 0 {
        return;
    }

    let lines = entry.map_or_else(
        || vec![Line::from("Journal entry is no longer available")],
        |entry| {
            let mut lines = vec![
                Line::from(format!(
                    "Time:     {}",
                    format_timestamp(entry.timestamp_micros)
                )),
                Line::from(format!("Source:   {}", entry.source)),
                Line::from(format!("Priority: {}", priority_label(entry.priority))),
                Line::from(""),
                Line::from("Message:"),
            ];
            if entry.message.is_empty() {
                lines.push(Line::from(""));
            } else {
                for line in entry.message.lines() {
                    lines.push(Line::from(line));
                }
            }
            lines.push(Line::from(""));
            lines.push(
                Line::from("Read-only inspection; Esc closes")
                    .style(Style::default().fg(Color::DarkGray)),
            );
            lines
        },
    );

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Log Details "),
        ),
        popup,
    );
}

fn priority_style(priority: Option<u8>) -> Style {
    match priority {
        Some(0..=3) => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Some(4) => Style::default().fg(Color::Yellow),
        Some(5 | 6) => Style::default().fg(Color::Cyan),
        Some(7) => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

fn format_timestamp(timestamp_micros: Option<u64>) -> String {
    let Some(seconds) = timestamp_micros.map(|value| value / 1_000_000) else {
        return "--:--:--".into();
    };
    let seconds = seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn single_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn formats_timestamp_and_priorities() {
        assert_eq!(format_timestamp(Some(3_723_000_000)), "01:02:03");
        assert_eq!(format_timestamp(None), "--:--:--");
        assert_eq!(priority_label(Some(3)), "error");
        assert_eq!(priority_label(Some(99)), "-");
    }

    #[test]
    fn table_messages_are_flattened_to_one_visual_row() {
        assert_eq!(single_line("first\n second\tthird"), "first second third");
    }

    #[test]
    fn detail_renders_multiline_messages_safely() {
        let entry = JournalEntry {
            id: 1,
            timestamp_micros: Some(1_000_000),
            source: "test".into(),
            priority: Some(3),
            message: "line1\nline2\nline3".into(),
        };
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_detail(frame, Some(&entry), Rect::new(0, 0, 100, 30));
            })
            .unwrap();
    }
}
