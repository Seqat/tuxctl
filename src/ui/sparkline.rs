//! A bar graph of a metric history: one column per sample, newest on the
//! right, `area.height` rows tall with eighth-block resolution.

use ratatui::{buffer::Buffer, layout::Rect, style::Style, widgets::Widget};

use crate::app::MetricHistory;

const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

pub(super) struct Graph<'a> {
    history: &'a MetricHistory,
    /// The value drawn as a full column.
    scale: f64,
    style: Style,
}

impl<'a> Graph<'a> {
    pub(super) fn new(history: &'a MetricHistory, scale: f64) -> Self {
        Self {
            history,
            scale,
            style: Style::default(),
        }
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
        for (offset, value) in samples.enumerate() {
            let fraction = if value.is_finite() {
                (value / scale).clamp(0.0, 1.0)
            } else {
                0.0
            };
            // Every sample shows at least its baseline, so a quiet metric still
            // reads as measured rather than missing.
            let eighths = ((fraction * (rows * 8) as f64).round() as usize).max(1);
            let x = first_column + offset as u16;
            for row in 0..rows {
                let filled = eighths.saturating_sub(row * 8).min(8);
                if filled == 0 {
                    break;
                }
                let y = area.bottom() - 1 - row as u16;
                buf[(x, y)]
                    .set_symbol(LEVELS[filled - 1])
                    .set_style(self.style);
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
}
