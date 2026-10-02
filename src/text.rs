//! Characters that must never reach the terminal from system data: C0, DEL and
//! C1 controls, and the bidirectional overrides that can disguise a name.
//! Shared by the TUI frame sanitizer and the plain-text `--check` report.

/// C0, DEL and C1 controls, plus the bidirectional embedding, override and
/// isolate characters that can make a name display as something else.
pub fn is_unsafe(character: char) -> bool {
    character.is_control() || matches!(character, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `text` with every line's unsafe characters removed; line breaks are kept.
pub fn strip_unsafe_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.extend(line.chars().filter(|&c| !is_unsafe(c)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_and_bidi_overrides_are_removed_but_lines_kept() {
        let text = "a\u{1b}]52;c;cm0=\u{7}b\nx\u{9b}2J\u{202E}y\r\n\u{2066}z\u{2069}\n";
        assert_eq!(strip_unsafe_lines(text), "a]52;c;cm0=b\nx2Jy\nz\n");
    }

    #[test]
    fn clean_text_is_unchanged() {
        let text = "sshd: ünïcode 漢字 ▲▼ ●\n  °C – \n";
        assert_eq!(strip_unsafe_lines(text), text);
    }
}
