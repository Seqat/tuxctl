//! The main menu opened by `Esc`, and the About page it leads to.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    about,
    action::{MenuItem, MouseTarget},
};

use super::layout;

const MENU_WIDTH: u16 = 34;
const HINT: &str = "↑↓ select  Enter open  Esc close";

/// Draws the menu and returns the rendered row of each item for hit testing.
pub(super) fn render_menu(
    frame: &mut Frame,
    selected: MenuItem,
    hovered: Option<&MouseTarget>,
    area: Rect,
) -> Vec<(MenuItem, Rect)> {
    // Borders, a blank line, the items, a blank line and the hint.
    let height = 4 + MenuItem::ALL.len() as u16 + 1;
    let popup = layout::centered_rect(area, MENU_WIDTH, height);
    if popup.width < 4 || popup.height < 3 {
        return Vec::new();
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", about::NAME));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let mut items = Vec::with_capacity(MenuItem::ALL.len());
    for (offset, item) in (1_u16..).zip(MenuItem::ALL) {
        if offset >= inner.height {
            break;
        }
        let row = Rect::new(inner.x, inner.y + offset, inner.width, 1);
        let style = if item == selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if hovered == Some(&MouseTarget::MenuItem(item)) {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(item.label())
                .style(style)
                .alignment(Alignment::Center),
            row,
        );
        items.push((item, row));
    }

    if inner.height > MenuItem::ALL.len() as u16 + 2 {
        let hint = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        frame.render_widget(
            Paragraph::new(layout::truncate(HINT, usize::from(inner.width)))
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            hint,
        );
    }
    items
}

pub(super) fn render_about(frame: &mut Frame, area: Rect) {
    let popup = layout::centered_rect(area, 60, 11);
    if popup.width == 0 || popup.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(format!("{} {}", about::NAME, about::VERSION)).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(about::DESCRIPTION),
        Line::from(""),
        Line::from(format!("License:  {}", about::LICENSE)),
        Line::from(format!("Source:   {}", about::REPOSITORY)),
        Line::from(format!(
            "Rust:     {} or newer to build",
            about::RUST_VERSION
        )),
        Line::from(""),
        Line::from("Esc back to the menu").style(Style::default().fg(Color::DarkGray)),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" About ")),
        popup,
    );
}
