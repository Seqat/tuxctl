//! The UI's colors, named by what they mean. Every screen takes its colors
//! from here, so a theme is a change to this module only.

use ratatui::style::Color;

/// Headings, titles, values and other highlights.
pub(super) const ACCENT: Color = Color::Cyan;
/// Hints, secondary text and inactive or unavailable items.
pub(super) const MUTED: Color = Color::DarkGray;
pub(super) const OK: Color = Color::Green;
pub(super) const WARNING: Color = Color::Yellow;
pub(super) const ERROR: Color = Color::Red;
/// Text on an `ERROR` background (a focused destructive button).
pub(super) const DANGER_FG: Color = Color::White;

/// The selected menu item, tab or button.
pub(super) const SELECTED_BG: Color = Color::Cyan;
pub(super) const SELECTED_FG: Color = Color::Black;
/// A hovered menu item, tab or button.
pub(super) const HOVER_BG: Color = Color::DarkGray;

/// The selected row of a table.
pub(super) const ROW_SELECTED_BG: Color = Color::DarkGray;
/// A hovered row of a table.
pub(super) const ROW_HOVER_BG: Color = Color::Rgb(35, 35, 35);

#[cfg(test)]
mod tests {
    /// Colors are only chosen here; tests may still compare against literals.
    #[test]
    fn screens_take_their_colors_from_the_theme() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("menu.rs", include_str!("menu.rs")),
            ("overview.rs", include_str!("overview.rs")),
            ("processes.rs", include_str!("processes.rs")),
            ("services.rs", include_str!("services.rs")),
            ("logs.rs", include_str!("logs.rs")),
            ("network.rs", include_str!("network.rs")),
            ("hardware.rs", include_str!("hardware.rs")),
            ("hardware_cpu.rs", include_str!("hardware_cpu.rs")),
            (
                "hardware_network_summary.rs",
                include_str!("hardware_network_summary.rs"),
            ),
            ("layout.rs", include_str!("layout.rs")),
            ("status.rs", include_str!("status.rs")),
        ];
        for (name, source) in sources {
            let code = source
                .split("\n#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or_default();
            assert!(
                !code.contains("Color::"),
                "{name} uses a literal color; add it to ui/theme.rs"
            );
        }
    }
}
