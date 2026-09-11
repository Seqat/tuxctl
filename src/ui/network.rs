use std::sync::Arc;

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::{
    action::MouseTarget,
    app::{calculate_scroll, App},
    linux::{NetworkInterfaceInfo, OperState},
};

use super::{format_bytes, layout};

pub struct NetworkRender {
    pub rows: Vec<(Arc<str>, Rect)>,
    pub scroll_area: Rect,
    pub start: usize,
    pub height: usize,
}

const COLUMN_SPACING: u16 = 1;

pub fn render(frame: &mut Frame, app: &App, area: Rect) -> NetworkRender {
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
        app.network_count(),
        app.selected_network_index(),
        app.network_scroll(),
        height,
    );
    let end = start.saturating_add(height).min(app.network_count());
    let selected = app.selected_network().map(|net| net.name.as_str());
    let hovered = app.hovered();
    let mut hit_rows = Vec::with_capacity(end.saturating_sub(start));

    let width = table_area.width;

    let rows = (start..end).filter_map(|index| {
        let iface = app.network_at(index)?;
        let offset = u16::try_from(index.saturating_sub(start)).ok()?;
        hit_rows.push((
            Arc::from(iface.name.as_str()),
            Rect::new(
                row_area.x,
                row_area.y.saturating_add(offset),
                row_area.width,
                1,
            ),
        ));

        let is_selected = selected == Some(iface.name.as_str());
        let is_hovered =
            matches!(hovered, Some(MouseTarget::NetworkRow(name)) if name.as_ref() == iface.name);

        let style = if is_selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else if is_hovered {
            Style::default().bg(Color::Rgb(35, 35, 35))
        } else {
            Style::default()
        };

        let (state_text, state_style) = match iface.operstate {
            OperState::Up => (
                "● up",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            OperState::Down => ("○ down", Style::default().fg(Color::DarkGray)),
            OperState::Dormant => ("◌ dormant", Style::default().fg(Color::Yellow)),
            _ => (iface.operstate.as_str(), Style::default().fg(Color::Yellow)),
        };

        let name_prefix = if is_selected { "> " } else { "  " };
        let name_cell = Cell::from(format!("{name_prefix}{}", iface.name));
        let state_cell = Cell::from(state_text).style(state_style);

        let primary_addr = if let Some(ipv4) = iface.ipv4_addresses.first() {
            ipv4.to_string()
        } else if let Some(ipv6) = iface.ipv6_addresses.first() {
            ipv6.to_string()
        } else {
            "--".to_string()
        };

        let rx_rate = format_rate(iface.rx_rate_bytes_per_sec);
        let tx_rate = format_rate(iface.tx_rate_bytes_per_sec);

        let cells = if width >= 110 {
            let ipv4_str = if iface.ipv4_addresses.is_empty() {
                "--".to_string()
            } else {
                iface
                    .ipv4_addresses
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let ipv6_str = if iface.ipv6_addresses.is_empty() {
                "--".to_string()
            } else {
                iface
                    .ipv6_addresses
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            vec![
                name_cell,
                state_cell,
                Cell::from(ipv4_str),
                Cell::from(ipv6_str),
                Cell::from(rx_rate),
                Cell::from(tx_rate),
                Cell::from(format_bytes(iface.rx_bytes)),
                Cell::from(format_bytes(iface.tx_bytes)),
            ]
        } else if width >= 85 {
            vec![
                name_cell,
                state_cell,
                Cell::from(primary_addr),
                Cell::from(rx_rate),
                Cell::from(tx_rate),
                Cell::from(format_bytes(iface.rx_bytes)),
                Cell::from(format_bytes(iface.tx_bytes)),
            ]
        } else if width >= 65 {
            vec![
                name_cell,
                state_cell,
                Cell::from(primary_addr),
                Cell::from(rx_rate),
                Cell::from(tx_rate),
            ]
        } else if width >= 45 {
            vec![
                name_cell,
                state_cell,
                Cell::from(rx_rate),
                Cell::from(tx_rate),
            ]
        } else {
            vec![name_cell, state_cell]
        };

        Some(Row::new(cells).style(style))
    });

    let (headers, widths) = if width >= 110 {
        (
            vec![
                "  INTERFACE",
                "STATE",
                "IPV4",
                "IPV6",
                "RX RATE",
                "TX RATE",
                "RX TOTAL",
                "TX TOTAL",
            ],
            vec![
                Constraint::Length(14),
                Constraint::Length(10),
                Constraint::Length(18),
                Constraint::Percentage(25),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Min(10),
            ],
        )
    } else if width >= 85 {
        (
            vec![
                "  INTERFACE",
                "STATE",
                "ADDRESS",
                "RX RATE",
                "TX RATE",
                "RX TOTAL",
                "TX TOTAL",
            ],
            vec![
                Constraint::Length(14),
                Constraint::Length(10),
                Constraint::Percentage(28),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Min(10),
            ],
        )
    } else if width >= 65 {
        (
            vec!["  INTERFACE", "STATE", "ADDRESS", "RX RATE", "TX RATE"],
            vec![
                Constraint::Length(14),
                Constraint::Length(10),
                Constraint::Min(16),
                Constraint::Length(12),
                Constraint::Length(12),
            ],
        )
    } else if width >= 45 {
        (
            vec!["  INTERFACE", "STATE", "RX RATE", "TX RATE"],
            vec![
                Constraint::Length(14),
                Constraint::Length(10),
                Constraint::Length(11),
                Constraint::Min(11),
            ],
        )
    } else {
        (
            vec!["  INTERFACE", "STATE"],
            vec![Constraint::Length(14), Constraint::Min(8)],
        )
    };

    let header_row = Row::new(headers).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let table = Table::new(rows, widths)
        .header(header_row)
        .column_spacing(COLUMN_SPACING)
        .flex(Flex::Start);
    frame.render_widget(table, table_area);

    if app.network_count() == 0 && row_area.height > 0 {
        let message = if app.network_error().is_some() {
            "Network data unavailable"
        } else {
            "No network interfaces found"
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            row_area,
        );
    }

    NetworkRender {
        rows: hit_rows,
        scroll_area: row_area,
        start,
        height,
    }
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::raw(" "),
        Span::raw(format!("{} interfaces", app.network_count())),
    ];

    if area.width >= 40 {
        spans.push(Span::raw("   Enter Details"));
    }

    if let Some(error) = app.network_error() {
        spans.push(Span::styled(
            format!("   Error: {error}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn render_detail(frame: &mut Frame, iface: Option<&NetworkInterfaceInfo>, area: Rect) {
    let popup = layout::centered_rect(area, 72, 17);
    if popup.width == 0 || popup.height == 0 {
        return;
    }

    let lines = iface.map_or_else(
        || {
            vec![
                Line::from("Interface is no longer available"),
                Line::from(""),
                Line::from("Read-only inspection; Esc closes")
                    .style(Style::default().fg(Color::DarkGray)),
            ]
        },
        |iface| {
            let ipv4_str = if iface.ipv4_addresses.is_empty() {
                "None".to_string()
            } else {
                iface
                    .ipv4_addresses
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let ipv6_str = if iface.ipv6_addresses.is_empty() {
                "None".to_string()
            } else {
                iface
                    .ipv6_addresses
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            vec![
                Line::from(format!("Interface:    {}", iface.name)),
                Line::from(format!("State:        {}", iface.operstate.as_str())),
                Line::from(format!(
                    "MAC Address:  {}",
                    iface.mac_address.as_deref().unwrap_or("N/A")
                )),
                Line::from(format!(
                    "MTU:          {}",
                    iface.mtu.map_or("N/A".to_string(), |m| m.to_string())
                )),
                Line::from(format!("IPv4:         {}", ipv4_str)),
                Line::from(format!("IPv6:         {}", ipv6_str)),
                Line::from(format!(
                    "RX Rate:      {}  |  TX Rate: {}",
                    format_rate(iface.rx_rate_bytes_per_sec),
                    format_rate(iface.tx_rate_bytes_per_sec)
                )),
                Line::from(format!(
                    "RX Total:     {}  ({} packets, {} errs, {} drop)",
                    format_bytes(iface.rx_bytes),
                    iface.rx_packets,
                    iface.rx_errors,
                    iface.rx_dropped
                )),
                Line::from(format!(
                    "TX Total:     {}  ({} packets, {} errs, {} drop)",
                    format_bytes(iface.tx_bytes),
                    iface.tx_packets,
                    iface.tx_errors,
                    iface.tx_dropped
                )),
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
                .title(" Network Interface Details "),
        ),
        popup,
    );
}

pub fn format_rate(rate: Option<f64>) -> String {
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];

    let Some(rate) = rate else {
        return "--".to_string();
    };

    if rate < 1.0 {
        return "0 B/s".to_string();
    }

    let mut value = rate;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rates_sensibly() {
        assert_eq!(format_rate(None), "--");
        assert_eq!(format_rate(Some(0.0)), "0 B/s");
        assert_eq!(format_rate(Some(842.0)), "842 B/s");
        assert_eq!(format_rate(Some(12.4 * 1024.0)), "12.4 KiB/s");
        assert_eq!(format_rate(Some(3.7 * 1024.0 * 1024.0)), "3.7 MiB/s");
        assert_eq!(
            format_rate(Some(1.2 * 1024.0 * 1024.0 * 1024.0)),
            "1.2 GiB/s"
        );
    }
}
