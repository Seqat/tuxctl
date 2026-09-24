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

/// Foreground of the screen behind a popup.
pub(super) const BACKDROP_FG: Color = Color::DarkGray;
pub(super) const BACKDROP_BG: Color = Color::Reset;

/// How many colors the terminal shows; decides how the bands below are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    /// The safe choice when nothing better is advertised.
    #[default]
    Ansi16,
}

impl ColorDepth {
    /// From `COLORTERM` (`truecolor`, `24bit`) and `TERM` (`*256color*`).
    pub fn detect(colorterm: Option<&str>, term: Option<&str>) -> Self {
        let colorterm = colorterm.unwrap_or_default().to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" {
            Self::TrueColor
        } else if term.is_some_and(|term| term.contains("256color")) {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }
}

static COLOR_DEPTH: std::sync::OnceLock<ColorDepth> = std::sync::OnceLock::new();

/// Sets the color depth once, at startup; rendering never reads the
/// environment. Unset (as in tests), bands use 16 colors.
pub fn init_color_depth(depth: ColorDepth) {
    let _ = COLOR_DEPTH.set(depth);
}

fn color_depth() -> ColorDepth {
    COLOR_DEPTH.get().copied().unwrap_or_default()
}

/// Utilization and temperature bands: [0, 10) light blue, [10, 65) green,
/// [65, 80) yellow, [80, 95) orange, 95 and above red. Temperatures are
/// placed as a percentage of their critical limit.
pub(super) fn band(percent: f64) -> Color {
    band_color(percent, color_depth())
}

pub(super) fn band_color(percent: f64, depth: ColorDepth) -> Color {
    // (true color, 256-color index, 16-color fallback); 16 colors have no
    // orange, so it becomes the bright red and red the normal one.
    const BANDS: [(Color, u8, Color); 5] = [
        (Color::Rgb(125, 180, 255), 111, Color::LightBlue),
        (Color::Rgb(95, 205, 120), 78, Color::Green),
        (Color::Rgb(235, 200, 70), 220, Color::Yellow),
        (Color::Rgb(255, 145, 50), 208, Color::LightRed),
        (Color::Rgb(240, 70, 70), 196, Color::Red),
    ];
    let index = match percent {
        percent if percent.is_nan() || percent < 10.0 => 0,
        percent if percent < 65.0 => 1,
        percent if percent < 80.0 => 2,
        percent if percent < 95.0 => 3,
        _ => 4,
    };
    let (true_color, indexed, basic) = BANDS[index];
    match depth {
        ColorDepth::TrueColor => true_color,
        ColorDepth::Ansi256 => Color::Indexed(indexed),
        ColorDepth::Ansi16 => basic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_change_at_10_65_80_and_95_percent() {
        let bands = [
            (-5.0, Color::LightBlue),
            (0.0, Color::LightBlue),
            (9.99, Color::LightBlue),
            (10.0, Color::Green),
            (64.99, Color::Green),
            (65.0, Color::Yellow),
            (79.99, Color::Yellow),
            (80.0, Color::LightRed),
            (94.99, Color::LightRed),
            (95.0, Color::Red),
            (150.0, Color::Red),
            (f64::NAN, Color::LightBlue),
        ];
        for (percent, color) in bands {
            assert_eq!(band_color(percent, ColorDepth::Ansi16), color, "{percent}");
        }
    }

    #[test]
    fn bands_use_the_colors_the_terminal_has() {
        assert_eq!(
            band_color(70.0, ColorDepth::TrueColor),
            Color::Rgb(235, 200, 70)
        );
        assert_eq!(band_color(70.0, ColorDepth::Ansi256), Color::Indexed(220));
        assert_eq!(band_color(70.0, ColorDepth::Ansi16), Color::Yellow);
        assert_eq!(band_color(90.0, ColorDepth::Ansi256), Color::Indexed(208));
    }

    #[test]
    fn color_depth_comes_from_colorterm_then_term() {
        for (colorterm, term, depth) in [
            (Some("truecolor"), Some("xterm"), ColorDepth::TrueColor),
            (Some("24bit"), None, ColorDepth::TrueColor),
            (Some("TrueColor"), None, ColorDepth::TrueColor),
            (None, Some("xterm-256color"), ColorDepth::Ansi256),
            (Some(""), Some("screen-256color"), ColorDepth::Ansi256),
            (None, Some("xterm"), ColorDepth::Ansi16),
            (None, Some("linux"), ColorDepth::Ansi16),
            (None, None, ColorDepth::Ansi16),
        ] {
            assert_eq!(
                ColorDepth::detect(colorterm, term),
                depth,
                "{colorterm:?} {term:?}"
            );
        }
    }

    /// Colors are only chosen here; tests may still compare against literals.
    #[test]
    fn screens_take_their_colors_from_the_theme() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("cards.rs", include_str!("cards.rs")),
            ("sparkline.rs", include_str!("sparkline.rs")),
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
