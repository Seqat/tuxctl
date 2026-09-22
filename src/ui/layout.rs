use ratatui::layout::Rect;

use crate::action::Tab;

pub const MIN_TERMINAL_WIDTH: u16 = 40;
pub const MIN_TERMINAL_HEIGHT: u16 = 15;

pub fn terminal_size_supported(area: Rect) -> bool {
    area.width >= MIN_TERMINAL_WIDTH && area.height >= MIN_TERMINAL_HEIGHT
}

pub struct ScreenLayout {
    pub tabs: Rect,
    pub content: Rect,
}

pub fn screen(area: Rect) -> ScreenLayout {
    let tabs_height = area.height.min(1);

    ScreenLayout {
        tabs: Rect::new(area.x, area.y, area.width, tabs_height),
        content: Rect::new(
            area.x,
            area.y.saturating_add(tabs_height),
            area.width,
            area.height.saturating_sub(tabs_height),
        ),
    }
}

/// Lays out tab labels, preferring in order: padded full labels, full labels
/// separated by one space, padded short labels, then space-separated short
/// labels. Adjacent labels are never drawn without separation; whatever does
/// not fit on the last option is clipped.
pub fn tab_areas(area: Rect) -> Vec<(Tab, Rect)> {
    const OPTIONS: [(bool, usize, usize); 4] =
        [(false, 1, 0), (false, 0, 1), (true, 1, 0), (true, 0, 1)];
    let (short, padding, gap) = OPTIONS
        .into_iter()
        .find(|&(short, padding, gap)| {
            tab_row_width(short, padding, gap) <= usize::from(area.width)
        })
        .unwrap_or(OPTIONS[OPTIONS.len() - 1]);

    let right = u32::from(area.x) + u32::from(area.width);
    let mut x = u32::from(area.x);
    let mut areas = Vec::with_capacity(Tab::ALL.len());
    for tab in Tab::ALL {
        let remaining = right.saturating_sub(x);
        if remaining == 0 {
            break;
        }

        let desired_width = (tab_label(tab, short).chars().count() + padding * 2) as u32;
        let width = desired_width.min(remaining).min(u32::from(u16::MAX)) as u16;
        let rect = Rect::new(
            x.min(u32::from(u16::MAX)) as u16,
            area.y,
            width,
            area.height,
        );
        areas.push((tab, rect));
        x = x
            .saturating_add(u32::from(width))
            .saturating_add(gap as u32);
    }

    areas
}

/// The label drawn in a tab area produced by [`tab_areas`].
pub fn tab_text(tab: Tab, width: u16) -> String {
    let width = usize::from(width);
    let full = tab.label();
    let short = tab.short_label();
    if width >= full.chars().count() + 2 {
        format!(" {full} ")
    } else if width >= full.chars().count() {
        full.to_owned()
    } else if width >= short.chars().count() + 2 {
        format!(" {short} ")
    } else {
        short.to_owned()
    }
}

fn tab_label(tab: Tab, short: bool) -> &'static str {
    if short {
        tab.short_label()
    } else {
        tab.label()
    }
}

fn tab_row_width(short: bool, padding: usize, gap: usize) -> usize {
    Tab::ALL
        .iter()
        .map(|&tab| tab_label(tab, short).chars().count() + padding * 2)
        .sum::<usize>()
        + gap * (Tab::ALL.len() - 1)
}

pub fn centered_rect(area: Rect, requested_width: u16, requested_height: u16) -> Rect {
    let width = requested_width.min(area.width);
    let height = requested_height.min(area.height);

    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

pub fn centered_rows(area: Rect, requested_height: u16) -> Rect {
    centered_rect(area, area.width, requested_height)
}

pub fn truncate(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".into();
    }

    let mut truncated = text.chars().take(width - 1).collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_layout_clamps_to_small_areas() {
        let layout = screen(Rect::new(0, 0, 2, 1));

        assert_eq!(layout.tabs, Rect::new(0, 0, 2, 1));
        assert_eq!(layout.content, Rect::new(0, 1, 2, 0));
    }

    #[test]
    fn tab_areas_are_clipped_to_available_width() {
        let area = Rect::new(5, 2, 12, 1);
        let tabs = tab_areas(area);

        assert_eq!(tabs[0], (Tab::Overview, Rect::new(5, 2, 3, 1)));
        assert_eq!(tabs[1], (Tab::Processes, Rect::new(9, 2, 4, 1)));
        assert_eq!(tabs[2], (Tab::Services, Rect::new(14, 2, 3, 1)));
        assert_eq!(tabs.len(), 3);
    }

    #[test]
    fn tab_areas_keep_labels_separated_as_width_shrinks() {
        let padded = tab_areas(Rect::new(1, 1, 46, 1));
        assert_eq!(padded[0], (Tab::Overview, Rect::new(1, 1, 10, 1)));
        assert_eq!(tab_text(Tab::Overview, padded[0].1.width), " Overview ");

        let spaced = tab_areas(Rect::new(1, 1, 40, 1));
        assert_eq!(spaced[0], (Tab::Overview, Rect::new(1, 1, 8, 1)));
        assert_eq!(spaced[1], (Tab::Processes, Rect::new(10, 1, 9, 1)));
        assert_eq!(tab_text(Tab::Processes, spaced[1].1.width), "Processes");

        let short = tab_areas(Rect::new(1, 1, 38, 1));
        assert_eq!(short.len(), Tab::ALL.len());
        assert_eq!(short[0], (Tab::Overview, Rect::new(1, 1, 5, 1)));
        assert_eq!(tab_text(Tab::Overview, short[0].1.width), " Ovr ");
        assert_eq!(short[4], (Tab::Network, Rect::new(23, 1, 5, 1)));
    }

    #[test]
    fn centers_content_without_exceeding_bounds() {
        let area = Rect::new(4, 2, 30, 9);

        assert_eq!(centered_rect(area, 20, 3), Rect::new(9, 5, 20, 3));
        assert_eq!(centered_rect(area, 40, 12), area);
    }

    #[test]
    fn truncates_long_text_without_splitting_characters() {
        assert_eq!(truncate("short", 8), "short");
        assert_eq!(truncate("processor model", 10), "processor…");
        assert_eq!(truncate("CPU λ model", 6), "CPU λ…");
        assert_eq!(truncate("anything", 1), "…");
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn minimum_terminal_boundary_requires_both_dimensions() {
        assert!(terminal_size_supported(Rect::new(0, 0, 40, 15)));
        assert!(!terminal_size_supported(Rect::new(0, 0, 39, 15)));
        assert!(!terminal_size_supported(Rect::new(0, 0, 40, 14)));
        assert!(!terminal_size_supported(Rect::new(0, 0, 39, 14)));
        assert!(!terminal_size_supported(Rect::new(0, 0, 0, 0)));
    }
}
