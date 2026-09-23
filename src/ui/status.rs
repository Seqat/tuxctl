//! One-line screen status: persistent interaction state first, then a transient
//! notice, then key hints only when they fit whole.
//!
//! Search and filter state is visible interaction state (CLAUDE.md regression
//! trap #6): a message or error is appended after it, never shown instead of it.
//! Overflow clips on the right, so the persistent part is the last to go.

use ratatui::text::{Line, Span};

const SEPARATOR: &str = "   ";

/// `persistent` is always shown (may be empty); `notice` follows it; the first
/// entry of `hints` that fits in `width` is added only when there is no notice.
pub(super) fn status_line(
    persistent: String,
    notice: Option<Span<'static>>,
    hints: &[&'static str],
    width: u16,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    let used = 1 + persistent.chars().count();
    let has_persistent = !persistent.is_empty();
    if has_persistent {
        spans.push(Span::raw(persistent));
    }

    let separator = |spans: &mut Vec<Span<'static>>| {
        if has_persistent {
            spans.push(Span::raw(SEPARATOR));
        }
    };
    if let Some(notice) = notice {
        separator(&mut spans);
        spans.push(notice);
    } else {
        let gap = if has_persistent { SEPARATOR.len() } else { 0 };
        if let Some(hint) = hints
            .iter()
            .find(|hint| used + gap + hint.chars().count() <= usize::from(width))
        {
            separator(&mut spans);
            spans.push(Span::raw(*hint));
        }
    }
    Line::from(spans)
}

/// Appends an active view filter to the persistent status text; like a
/// search, it is interaction state and must stay visible.
pub(super) fn with_view(persistent: String, view: Option<&str>) -> String {
    match view {
        Some(view) if persistent.is_empty() => format!("View: {view}"),
        Some(view) => format!("{persistent}   View: {view}"),
        None => persistent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn notice_follows_persistent_state_instead_of_replacing_it() {
        let line = status_line(
            "Filter: \"ssh\"".into(),
            Some(Span::raw("Sent SIGTERM to sshd (42)")),
            &["/ edit   Esc clear"],
            120,
        );
        assert_eq!(text(&line), " Filter: \"ssh\"   Sent SIGTERM to sshd (42)");
    }

    #[test]
    fn hints_use_the_first_option_that_fits_and_never_split() {
        let hints = [
            "/ search   Enter details   r refresh",
            "/ search   r refresh",
        ];
        assert_eq!(
            text(&status_line("12 services".into(), None, &hints, 80)),
            " 12 services   / search   Enter details   r refresh"
        );
        assert_eq!(
            text(&status_line("12 services".into(), None, &hints, 40)),
            " 12 services   / search   r refresh"
        );
        assert_eq!(
            text(&status_line("12 services".into(), None, &hints, 20)),
            " 12 services"
        );
    }

    #[test]
    fn an_active_view_stays_visible_next_to_a_notice() {
        let persistent = with_view("Filter: \"ssh\"".into(), Some("no kernel threads"));
        let line = status_line(persistent, Some(Span::raw("Sent SIGTERM")), &[], 120);
        assert_eq!(
            text(&line),
            " Filter: \"ssh\"   View: no kernel threads   Sent SIGTERM"
        );
        assert_eq!(with_view("12 services".into(), None), "12 services");
    }

    #[test]
    fn empty_persistent_state_starts_with_hints() {
        assert_eq!(
            text(&status_line(
                String::new(),
                None,
                &["/ find   Enter view"],
                40
            )),
            " / find   Enter view"
        );
    }
}
