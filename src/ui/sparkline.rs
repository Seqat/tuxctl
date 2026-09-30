//! A bar graph of a metric history: one column per sample, newest on the
//! right, `area.height` rows tall with eighth-block resolution.

use ratatui::{buffer::Buffer, layout::Rect, style::Style, widgets::Widget};

use crate::app::MetricHistory;

use super::theme;

const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

pub(super) struct Graph<'a> {
    history: &'a MetricHistory,
    /// The value drawn as a full column.
    scale: f64,
    /// Each column takes the utilization band of its own value; otherwise the
    /// graph is neutral (for values without a meaningful percentage).
    banded: bool,
    /// Heights follow `ln(1 + value / unit)`, so one spike does not flatten
    /// the rest; values well below `unit` stay near the baseline.
    log_unit: Option<f64>,
}

impl<'a> Graph<'a> {
    pub(super) fn new(history: &'a MetricHistory, scale: f64) -> Self {
        Self {
            history,
            scale,
            banded: false,
            log_unit: None,
        }
    }

    /// Draws heights on a logarithmic scale of `unit` (see `log_unit`).
    pub(super) fn logarithmic(mut self, unit: f64) -> Self {
        self.log_unit = (unit.is_finite() && unit > 0.0).then_some(unit);
        self
    }

    /// Colors each column by the band of its value as a share of `scale`.
    pub(super) fn banded(mut self) -> Self {
        self.banded = true;
        self
    }
}

impl Widget for Graph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let scale = if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        };
        let width = usize::from(area.width);
        let rows = usize::from(area.height);
        let samples = self.history.iter();
        let shown = samples.len().min(width);
        let samples = samples.skip(self.history.iter().len() - shown);
        let first_column = area.x + (width - shown) as u16;
        let fraction_of = |value: f64| match self.log_unit {
            Some(unit) => (value / unit).max(0.0).ln_1p() / (scale / unit).ln_1p(),
            None => value / scale,
        };
        for (offset, value) in samples.enumerate() {
            let fraction = if value.is_finite() {
                fraction_of(value).clamp(0.0, 1.0)
            } else {
                0.0
            };
            // Every sample shows at least its baseline, so a quiet metric still
            // reads as measured rather than missing.
            let eighths = ((fraction * (rows * 8) as f64).round() as usize).max(1);
            let style = if self.banded {
                Style::default().fg(theme::band(fraction * 100.0))
            } else {
                Style::default()
            };
            let x = first_column + offset as u16;
            for row in 0..rows {
                let filled = eighths.saturating_sub(row * 8).min(8);
                if filled == 0 {
                    break;
                }
                let y = area.bottom() - 1 - row as u16;
                buf[(x, y)].set_symbol(LEVELS[filled - 1]).set_style(style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(values: &[f64]) -> MetricHistory {
        let mut history = MetricHistory::default();
        for &value in values {
            history.push_for_test(value);
        }
        history
    }

    fn draw(values: &[f64], scale: f64, width: u16, height: u16) -> Vec<String> {
        let history = history(values);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        Graph::new(&history, scale).render(area, &mut buffer);
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn samples_are_right_aligned_with_the_newest_last() {
        assert_eq!(draw(&[0.0, 50.0, 100.0], 100.0, 5, 1), ["  ▁▄█"]);
    }

    #[test]
    fn columns_span_rows_with_eighth_blocks() {
        // 2 rows = 16 eighths: 25 % is 4 eighths, 75 % is 12 (one full row + 4).
        assert_eq!(draw(&[25.0, 75.0, 100.0], 100.0, 3, 2), [" ▄█", "▄██"]);
    }

    #[test]
    fn only_the_newest_samples_that_fit_are_drawn() {
        assert_eq!(draw(&[100.0, 0.0, 0.0], 100.0, 2, 1), ["▁▁"]);
    }

    #[test]
    fn empty_areas_invalid_scales_and_values_are_safe() {
        assert_eq!(draw(&[50.0], 100.0, 0, 0), Vec::<String>::new());
        assert_eq!(draw(&[], 100.0, 3, 1), ["   "]);
        assert_eq!(
            draw(&[2.0], 0.0, 1, 1),
            ["█"],
            "a zero scale draws against 1"
        );
        assert_eq!(draw(&[2.0], f64::NAN, 1, 1), ["█"]);
        assert_eq!(
            draw(&[500.0], 100.0, 1, 1),
            ["█"],
            "values above the scale are capped"
        );
    }

    #[test]
    fn a_logarithmic_graph_keeps_everyday_values_visible_beside_a_spike() {
        const MIB: f64 = 1024.0 * 1024.0;
        let values = [0.0, 512.0, 10.0 * 1024.0, MIB];
        let history = history(&values);
        let area = Rect::new(0, 0, 4, 1);
        let text = |graph: Graph| {
            let mut buffer = Buffer::empty(area);
            graph.render(area, &mut buffer);
            (0..4).map(|x| buffer[(x, 0)].symbol()).collect::<String>()
        };
        assert_eq!(text(Graph::new(&history, MIB)), "▁▁▁█");
        // 10 KiB/s reaches about a third; below 1 KiB/s stays at the baseline.
        assert_eq!(text(Graph::new(&history, MIB).logarithmic(1024.0)), "▁▁▃█");
        assert_eq!(
            text(Graph::new(&history, MIB).logarithmic(0.0)),
            "▁▁▁█",
            "an invalid unit keeps the linear scale"
        );
    }

    #[test]
    fn banded_columns_take_the_color_of_their_own_value() {
        use ratatui::style::Color;
        let history = history(&[5.0, 50.0, 70.0, 90.0, 99.0]);
        let area = Rect::new(0, 0, 5, 2);
        let mut buffer = Buffer::empty(area);
        Graph::new(&history, 100.0)
            .banded()
            .render(area, &mut buffer);
        let bottom: Vec<Color> = (0..5).map(|x| buffer[(x, 1)].fg).collect();
        assert_eq!(
            bottom,
            [
                Color::LightBlue,
                Color::Green,
                Color::Yellow,
                Color::LightRed,
                Color::Red
            ]
        );
        // Every cell of a column shares its color.
        assert_eq!(buffer[(4, 0)].fg, Color::Red);

        let mut neutral = Buffer::empty(area);
        Graph::new(&history, 100.0).render(area, &mut neutral);
        assert!((0..5).all(|x| neutral[(x, 1)].fg == Color::Reset));
    }
}
