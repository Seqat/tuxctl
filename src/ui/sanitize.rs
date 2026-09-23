//! The last step of every frame: no control character reaches the terminal.
//!
//! Process names, command lines, journal messages and other system data are
//! controlled by other local users. Ratatui only drops `\n` when rendering
//! spans, and the backend prints cell symbols as they are, so an embedded
//! escape sequence would be sent to the terminal (clipboard writes via OSC 52,
//! a full reset, fake links). Every string reaches the terminal through the
//! frame buffer, so cleaning the buffer covers every screen and overlay.

use ratatui::buffer::Buffer;

/// Shown in place of a cell that held only control characters.
const REPLACEMENT: &str = "\u{FFFD}";

/// Removes control characters and bidirectional overrides from every cell.
/// A cell left empty shows [`REPLACEMENT`]; cells without such characters are
/// not touched.
pub(super) fn sanitize_buffer(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if !symbol.chars().any(is_unsafe) {
            continue;
        }
        let cleaned: String = symbol.chars().filter(|&c| !is_unsafe(c)).collect();
        if cleaned.is_empty() {
            cell.set_symbol(REPLACEMENT);
        } else {
            cell.set_symbol(&cleaned);
        }
    }
}

/// C0, DEL and C1 controls, plus the bidirectional embedding, override and
/// isolate characters that can make a name display as something else.
fn is_unsafe(character: char) -> bool {
    character.is_control() || matches!(character, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
pub(super) fn has_unsafe_symbol(buffer: &Buffer) -> bool {
    buffer
        .content
        .iter()
        .any(|cell| cell.symbol().chars().any(is_unsafe))
}

#[cfg(test)]
mod tests {
    use ratatui::{layout::Rect, text::Line, widgets::Widget};

    use super::*;

    fn rendered(text: &str, width: u16) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
        Line::from(text.to_owned()).render(buffer.area, &mut buffer);
        buffer
    }

    fn symbols(buffer: &Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn unsanitized_spans_keep_escape_characters() {
        // The reason this module exists: ratatui passes them through.
        assert!(has_unsafe_symbol(&rendered("a\u{1b}]52;c;cm0=\u{7}b", 20)));
    }

    #[test]
    fn escape_sequences_are_removed_from_every_cell() {
        for hostile in [
            "a\u{1b}]52;c;cm0gLXJmIH4=\u{7}b",
            "\u{1b}c",
            "x\u{9b}2Jy",
            "tab\there",
            "cr\rlf",
            "abc\u{202E}fed",
            "\u{2066}iso\u{2069}",
            "\u{7f}del",
        ] {
            let mut buffer = rendered(hostile, 40);
            sanitize_buffer(&mut buffer);
            assert!(!has_unsafe_symbol(&buffer), "{hostile:?}");
        }
    }

    #[test]
    fn clean_cells_are_left_unchanged() {
        let text = "sshd: ünïcode 漢字 ▲▼ ●";
        let mut buffer = rendered(text, 40);
        let before = buffer.clone();

        sanitize_buffer(&mut buffer);

        assert_eq!(buffer, before);
        assert!(symbols(&buffer).starts_with("sshd: ünïcode 漢"));
    }

    #[test]
    fn visible_text_around_controls_is_kept_in_place() {
        let mut buffer = rendered("ab\u{1b}cd", 10);
        sanitize_buffer(&mut buffer);

        let text = symbols(&buffer);
        assert!(text.starts_with("ab"), "{text:?}");
        assert!(text.contains("cd"), "{text:?}");
        assert!(!text.contains('\u{1b}'));
        assert_eq!(buffer.content.len(), 10, "layout width is unchanged");
    }
}
