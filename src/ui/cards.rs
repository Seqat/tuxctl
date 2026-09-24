//! Overview cards: a bordered block whose top border carries the title
//! (`CPU  Ryzen 5 7500F · 45°C`) and whose bottom border can carry a note
//! such as the time span of the card's graph.

use std::time::Duration;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
    Frame,
};

use crate::app::MetricHistory;

use super::{hardware_cpu::format_window, sparkline::Graph, theme};

/// Graph rows a card gets when there is room; it grows further with spare
/// height and shrinks to one row before a card gives up its fixed rows.
pub(super) const GRAPH_ROWS: u16 = 4;
const SEPARATOR: &str = " · ";
/// An inline graph in a title row is left out when narrower than this.
const MIN_INLINE_GRAPH_WIDTH: u16 = 6;

/// Which optional title parts give way first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TitleOrder {
    /// The model shortens and goes first, then the parts from the last:
    /// `CPU  <model> · <temp> · <power>`.
    ModelFirst,
    /// The parts go first, from the last; the subject (an interface name)
    /// stays: `Network  <iface> · ● up · <temp>`.
    PartsFirst,
}

pub(super) struct CardTitle<'a> {
    pub name: &'static str,
    /// The model or subject; empty when unknown.
    pub model: &'a str,
    /// Values after the model, such as the temperature, each already styled
    /// (a part can have several spans: `P0 45°C  P1 47°C`).
    pub parts: Vec<Vec<Span<'static>>>,
    pub order: TitleOrder,
}

impl<'a> CardTitle<'a> {
    pub(super) fn plain(name: &'static str) -> Self {
        Self {
            name,
            model: "",
            parts: Vec::new(),
            order: TitleOrder::ModelFirst,
        }
    }
}

/// The widest title that fits `width` columns, including its surrounding
/// spaces. Parts that do not fit are left out, never shown as placeholders.
pub(super) fn title_line(title: &CardTitle, width: usize) -> Line<'static> {
    let models = if title.model.is_empty() {
        vec![String::new()]
    } else {
        let mut models = model_variants(title.model);
        models.push(String::new());
        models
    };
    let part_counts: Vec<usize> = (0..=title.parts.len()).rev().collect();
    let candidates: Vec<(&str, usize)> = match title.order {
        TitleOrder::ModelFirst => {
            let full_parts = title.parts.len();
            let mut candidates: Vec<_> = models
                .iter()
                .map(|model| (model.as_str(), full_parts))
                .collect();
            candidates.extend(part_counts.iter().skip(1).map(|&count| ("", count)));
            candidates
        }
        TitleOrder::PartsFirst => {
            let mut candidates: Vec<_> = part_counts
                .iter()
                .map(|&count| (models[0].as_str(), count))
                .collect();
            candidates.extend(models.iter().skip(1).map(|model| (model.as_str(), 0)));
            candidates
        }
    };
    for (model, parts) in candidates {
        let line = build_title(title.name, model, &title.parts[..parts]);
        if line.width() <= width {
            return line;
        }
    }
    // A name cut to `…` says nothing; leave the border plain instead.
    Line::default()
}

fn build_title(name: &'static str, model: &str, parts: &[Vec<Span<'static>>]) -> Line<'static> {
    let mut spans = vec![Span::raw(" "), Span::styled(name, name_style())];
    let mut first = true;
    let mut separator = |spans: &mut Vec<Span<'static>>| {
        spans.push(Span::raw(if first { "  " } else { SEPARATOR }));
        first = false;
    };
    if !model.is_empty() {
        separator(&mut spans);
        spans.push(Span::raw(model.to_owned()));
    }
    for part in parts {
        separator(&mut spans);
        spans.extend(part.iter().cloned());
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn name_style() -> Style {
    Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// The model as given, then without vendor noise, then its distinctive tail:
/// `AMD Ryzen 5 7500F 6-Core Processor` → `Ryzen 5 7500F` → `7500F`.
pub(super) fn model_variants(model: &str) -> Vec<String> {
    let short = short_model(model);
    let shortest = shortest_model(&short);
    let mut variants = vec![model.trim().to_owned()];
    for variant in [short, shortest] {
        if !variant.is_empty() && !variants.contains(&variant) {
            variants.push(variant);
        }
    }
    variants
}

fn short_model(model: &str) -> String {
    const NOISE: [&str; 11] = [
        "amd",
        "intel",
        "intel(r)",
        "core(tm)",
        "(r)",
        "(tm)",
        "nvidia",
        "geforce",
        "corporation",
        "processor",
        "cpu",
    ];
    // `… CPU @ 3.60GHz`, `… with Radeon Graphics`: the rest is not the model.
    let model = model
        .split(" @ ")
        .next()
        .unwrap_or_default()
        .split(" with ")
        .next()
        .unwrap_or_default();
    model
        .split_whitespace()
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            !NOISE.contains(&lower.as_str())
                && !lower.strip_suffix("-core").is_some_and(|count| {
                    !count.is_empty() && count.chars().all(|c| c.is_ascii_digit())
                })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// From the longest word with a digit (the model number) to the end:
/// `Ryzen 5 7500F` → `7500F`, `RTX 5070 Ti` → `5070 Ti`.
fn shortest_model(short: &str) -> String {
    let words: Vec<&str> = short.split_whitespace().collect();
    let Some(start) = words
        .iter()
        .enumerate()
        .filter(|(_, word)| word.chars().any(|c| c.is_ascii_digit()))
        .max_by_key(|(index, word)| (word.chars().count(), std::cmp::Reverse(*index)))
        .map(|(index, _)| index)
    else {
        return String::new();
    };
    words[start..].join(" ")
}

/// Rows for a card's graph and its optional rows (a CPU grid, more
/// interfaces) within `height`, after its `fixed` rows. The optional rows give
/// way first, then the graph shrinks to one row; spare rows go to the graph.
/// Without a graph area every free row is offered to the optional rows.
pub(super) fn split_rows(height: u16, fixed: u16, optional: u16, graph: bool) -> (u16, u16) {
    let free = height.saturating_sub(fixed);
    if !graph {
        return (0, free.min(optional));
    }
    if free >= GRAPH_ROWS + optional {
        (free - optional, optional)
    } else if free > GRAPH_ROWS {
        (GRAPH_ROWS, free - GRAPH_ROWS)
    } else {
        (free, 0)
    }
}

/// A graph shown on a card: its history, the value drawn as a full column,
/// and the sampling interval, for the time span in the bottom border.
pub(super) struct CardGraph<'a> {
    pub history: &'a MetricHistory,
    pub scale: f64,
    pub interval: Duration,
}

/// Draws a card with an optional graph, above its rows when `graphs` is set
/// and otherwise in the title row. Returns the area for the `fixed` and
/// optional rows, and how many optional rows it has room for.
pub(super) fn render_graph_card(
    frame: &mut Frame,
    area: Rect,
    title: &CardTitle,
    graph: Option<CardGraph>,
    graphs: bool,
    fixed: u16,
    optional: u16,
) -> (Rect, u16) {
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);
    let (graph_rows, optional_rows) =
        split_rows(inner_height, fixed, optional, graphs && graph.is_some());
    // The time span of the columns that fit: the graph is as wide as the card.
    let footer = graph.as_ref().map(|graph| {
        let samples = usize::from(inner_width).min(graph.history.capacity());
        format_window(graph.interval.saturating_mul(samples as u32))
    });
    let inner = render_card(frame, area, title, footer.as_deref());
    if let Some(graph) = &graph {
        if graph_rows > 0 {
            frame.render_widget(
                Graph::new(graph.history, graph.scale),
                Rect::new(inner.x, inner.y, inner.width, graph_rows.min(inner.height)),
            );
        } else if !graphs {
            render_title_graph(frame, area, title, graph.history, graph.scale);
        }
    }
    let rows = Rect::new(
        inner.x,
        inner.y.saturating_add(graph_rows),
        inner.width,
        inner.height.saturating_sub(graph_rows),
    );
    (rows, optional_rows)
}

/// Draws the card's border and title and returns the area inside it.
pub(super) fn render_card(
    frame: &mut Frame,
    area: Rect,
    title: &CardTitle,
    footer: Option<&str>,
) -> Rect {
    let title = title_line(title, usize::from(area.width.saturating_sub(2)));
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if let Some(footer) =
        footer.filter(|footer| footer.chars().count() + 4 <= usize::from(area.width))
    {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {footer} "),
                Style::default().fg(theme::MUTED),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// A one-row graph drawn into the top border after the title, for compact
/// cards; left out when too little of the border is free.
pub(super) fn render_title_graph(
    frame: &mut Frame,
    area: Rect,
    title: &CardTitle,
    history: &MetricHistory,
    scale: f64,
) {
    let title_width = title_line(title, usize::from(area.width.saturating_sub(2))).width() as u16;
    // Corner, title, a gap, the graph, a gap, corner.
    let x = area.x.saturating_add(1 + title_width + 1);
    let width = area.right().saturating_sub(2).saturating_sub(x);
    if width >= MIN_INLINE_GRAPH_WIDTH && area.height > 0 {
        frame.render_widget(Graph::new(history, scale), Rect::new(x, area.y, width, 1));
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

    fn cpu_title() -> CardTitle<'static> {
        CardTitle {
            name: "CPU",
            model: "AMD Ryzen 5 7500F 6-Core Processor",
            parts: vec![vec![Span::raw("45°C")], vec![Span::raw("15W")]],
            order: TitleOrder::ModelFirst,
        }
    }

    #[test]
    fn models_shorten_to_their_distinctive_part() {
        for (model, short, shortest) in [
            (
                "AMD Ryzen 5 7500F 6-Core Processor",
                "Ryzen 5 7500F",
                "7500F",
            ),
            ("Intel(R) Core(TM) i7-12700K", "i7-12700K", "i7-12700K"),
            (
                "Intel(R) Core(TM) i5-4590 CPU @ 3.30GHz",
                "i5-4590",
                "i5-4590",
            ),
            (
                "AMD Ryzen 7 5800H with Radeon Graphics",
                "Ryzen 7 5800H",
                "5800H",
            ),
            ("NVIDIA GeForce RTX 5070 Ti", "RTX 5070 Ti", "5070 Ti"),
            ("AMD Radeon RX 7900 XTX", "Radeon RX 7900 XTX", "7900 XTX"),
        ] {
            assert_eq!(short_model(model), short, "{model}");
            assert_eq!(shortest_model(short), shortest, "{model}");
        }
        assert_eq!(shortest_model("Virtual GPU"), "");
        assert_eq!(model_variants("Apple M2"), ["Apple M2", "M2"]);
    }

    #[test]
    fn titles_shorten_the_model_first_and_drop_temperature_last() {
        let title = cpu_title();
        let at = |width| text(&title_line(&title, width));
        assert_eq!(
            at(80),
            " CPU  AMD Ryzen 5 7500F 6-Core Processor · 45°C · 15W "
        );
        assert_eq!(at(35), " CPU  Ryzen 5 7500F · 45°C · 15W ");
        assert_eq!(at(25), " CPU  7500F · 45°C · 15W ");
        assert_eq!(at(20), " CPU  45°C · 15W ");
        assert_eq!(at(12), " CPU  45°C ");
        assert_eq!(at(7), " CPU ");
        assert_eq!(at(4), "", "not even the name fits");
        assert_eq!(at(2), "");
        for width in 0..90 {
            assert!(title_line(&title, width).width() <= width, "{width}");
        }
    }

    #[test]
    fn subject_titles_drop_their_parts_before_the_subject() {
        let title = CardTitle {
            name: "Network",
            model: "enp6s0",
            parts: vec![vec![Span::raw("● up")], vec![Span::raw("40°C")]],
            order: TitleOrder::PartsFirst,
        };
        let at = |width| text(&title_line(&title, width));
        assert_eq!(at(40), " Network  enp6s0 · ● up · 40°C ");
        assert_eq!(at(28), " Network  enp6s0 · ● up ");
        assert_eq!(at(20), " Network  enp6s0 ");
        assert_eq!(at(12), " Network ");
    }

    #[test]
    fn optional_rows_give_way_before_the_graph_shrinks() {
        // 1 fixed row, 3 optional rows.
        assert_eq!(
            split_rows(20, 1, 3, true),
            (16, 3),
            "spare rows go to the graph"
        );
        assert_eq!(split_rows(8, 1, 3, true), (4, 3));
        assert_eq!(split_rows(6, 1, 3, true), (4, 1));
        assert_eq!(
            split_rows(5, 1, 3, true),
            (4, 0),
            "optional rows hidden first"
        );
        assert_eq!(split_rows(3, 1, 3, true), (2, 0));
        assert_eq!(split_rows(2, 1, 3, true), (1, 0));
        assert_eq!(split_rows(1, 1, 3, true), (0, 0));
        assert_eq!(split_rows(0, 1, 3, true), (0, 0));
        assert_eq!(split_rows(9, 1, 3, false), (0, 3));
        assert_eq!(split_rows(3, 1, 3, false), (0, 2));
    }

    #[test]
    fn a_title_without_optional_parts_is_just_the_name() {
        assert_eq!(
            text(&title_line(&CardTitle::plain("Memory"), 40)),
            " Memory "
        );
    }
}
