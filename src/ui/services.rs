use std::sync::Arc;

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::{
    action::MouseTarget,
    app::{calculate_scroll, App},
    linux::ServiceInfo,
};

use super::layout;

pub struct ServiceRender {
    pub rows: Vec<(Arc<str>, Rect)>,
    pub scroll_area: Rect,
    pub start: usize,
    pub height: usize,
}

const COLUMN_SPACING: u16 = 1;
const COLUMN_WIDTHS: [Constraint; 5] = [
    Constraint::Percentage(34),
    Constraint::Length(10),
    Constraint::Length(12),
    Constraint::Length(14),
    Constraint::Min(12),
];

pub fn render(frame: &mut Frame, app: &App, area: Rect) -> ServiceRender {
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
    let start = calculate_scroll(
        app.service_count(),
        app.selected_service_index(),
        app.service_scroll(),
        height,
    );
    let end = start.saturating_add(height).min(app.service_count());
    let selected = app.selected_service().map(|service| service.unit.as_str());
    let hovered = app.hovered();
    let mut hit_rows = Vec::with_capacity(end.saturating_sub(start));

    let rows = (start..end).filter_map(|index| {
        let service = app.service_at(index)?;
        let offset = u16::try_from(index.saturating_sub(start)).ok()?;
        hit_rows.push((
            Arc::from(service.unit.as_str()),
            Rect::new(
                row_area.x,
                row_area.y.saturating_add(offset),
                row_area.width,
                1,
            ),
        ));

        let style = if selected == Some(service.unit.as_str()) {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else if matches!(hovered, Some(MouseTarget::ServiceRow(unit)) if unit.as_ref() == service.unit) {
            Style::default().bg(Color::Rgb(35, 35, 35))
        } else {
            Style::default()
        };

        let (state_icon, state_style) = match service.active_state.as_str() {
            "active" => ("●", Style::default().fg(Color::Green)),
            "failed" => ("✖", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            "inactive" | "deactivating" => ("○", Style::default().fg(Color::DarkGray)),
            "activating" | "reloading" => ("◌", Style::default().fg(Color::Yellow)),
            _ => ("?", Style::default().fg(Color::Yellow)),
        };
        let active_cell = Cell::from(format!("{state_icon} {}", service.active_state.as_str()))
            .style(state_style);

        Some(
            Row::new([
                Cell::from(format!(
                    "{}{}",
                    if selected == Some(service.unit.as_str()) {
                        "> "
                    } else {
                        "  "
                    },
                    service.unit
                )),
                Cell::from(service.load_state.as_str()),
                active_cell,
                Cell::from(service.sub_state.as_str()),
                Cell::from(service.description.as_str()),
            ])
            .style(style),
        )
    });

    let header = Row::new(["UNIT", "LOAD", "ACTIVE", "SUB", "DESCRIPTION"]).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(rows, COLUMN_WIDTHS)
        .header(header)
        .column_spacing(COLUMN_SPACING)
        .flex(Flex::Start);
    frame.render_widget(table, table_area);

    if app.service_count() == 0 && row_area.height > 0 {
        let message = if app.service_error().is_some() {
            "Service data unavailable"
        } else if app.service_search_query().is_empty() {
            "No system services found"
        } else {
            "No matching services"
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            row_area,
        );
    }

    ServiceRender {
        rows: hit_rows,
        scroll_area: row_area,
        start,
        height,
    }
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let text = if app.service_refreshing() {
        " Refreshing system services…".to_owned()
    } else if let Some(error) = app.service_error() {
        format!(" Refresh error: {error}   r retry")
    } else if app.service_searching() {
        format!(" Search: {}_", app.service_search_query())
    } else if app.service_search_query().is_empty() {
        if area.width >= 70 {
            format!(
                " {} services   / search   Enter details   r refresh",
                app.service_count()
            )
        } else {
            format!(" {} services   / search   r refresh", app.service_count())
        }
    } else if area.width >= 70 {
        format!(
            " Filter: \"{}\" ({} matches)   / edit   Esc clear   Enter details",
            app.service_search_query(),
            app.service_count()
        )
    } else {
        format!(
            " Filter: \"{}\" ({} matches)   Esc clear",
            app.service_search_query(),
            app.service_count()
        )
    };
    frame.render_widget(Paragraph::new(text), area);
}

pub fn render_detail(frame: &mut Frame, service: Option<&ServiceInfo>, area: Rect) {
    let popup = layout::centered_rect(area, 76, 9);
    if popup.width == 0 || popup.height == 0 {
        return;
    }

    let lines = service.map_or_else(
        || vec![Line::from("Service is no longer available")],
        |service| {
            vec![
                Line::from(format!("Unit:        {}", service.unit)),
                Line::from(format!("Description: {}", fallback(&service.description))),
                Line::from(format!("Load:        {}", fallback(&service.load_state))),
                Line::from(format!("Active:      {}", fallback(&service.active_state))),
                Line::from(format!("Sub:         {}", fallback(&service.sub_state))),
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
                .title(" Service Details "),
        ),
        popup,
    );
}

fn fallback(value: &str) -> &str {
    if value.is_empty() {
        "N/A"
    } else {
        value
    }
}
