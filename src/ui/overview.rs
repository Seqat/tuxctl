//! The Overview: responsive cards for CPU, GPU, memory, network, storage and
//! pinned processes, plus the system summary shown in the top border.

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{action::Action, app::App, keymap};

use super::{
    cards::{self, CardTitle},
    format_bytes, format_uptime, hardware, hardware_cpu, hardware_network_summary, layout, theme,
};

/// Content widths (the terminal is two columns wider) at which the Overview
/// switches from one column to a 2×2 grid, and adds a Pinned column.
pub(super) const MEDIUM_MIN_WIDTH: u16 = 98;
pub(super) const WIDE_MIN_WIDTH: u16 = 148;
/// A card is a border plus at least one row.
pub(super) const CARD_MIN_HEIGHT: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WidthMode {
    /// One column; graphs sit in the card titles unless every card fits
    /// with its graph above its rows.
    Narrow,
    /// A 2×2 grid, then Storage and Pinned side by side.
    Medium,
    /// A 2×2 grid and Storage, with Pinned as a column on the right.
    Wide,
}

impl WidthMode {
    pub(super) fn for_width(width: u16) -> Self {
        if width >= WIDE_MIN_WIDTH {
            Self::Wide
        } else if width >= MEDIUM_MIN_WIDTH {
            Self::Medium
        } else {
            Self::Narrow
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Card {
    Cpu,
    Gpu,
    Memory,
    Network,
    Storage,
    Pinned,
}

/// Heights a card needs, borders included, at each step of the height
/// compression. From the tallest down, a short Overview first hides the
/// optional rows (the CPU grid, more GPUs or interfaces), then shrinks the
/// graphs to one row, then cuts lists (Pinned, Storage) to one row, then
/// leaves out whole cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CardHeight {
    /// The fixed rows, one graph row and one list row.
    pub min: u16,
    /// With the whole list.
    pub listed: u16,
    /// With the whole list and full graphs.
    pub graphed: u16,
    /// With everything, the optional rows included.
    pub desired: u16,
    /// Whether the card has a graph, which takes a share of the spare rows
    /// once everything is shown.
    pub graph: bool,
}

impl CardHeight {
    /// `fixed` rows always shown, `optional` rows that give way first, and a
    /// graph when `graph` is set (one row at least, [`cards::GRAPH_ROWS`]
    /// when there is room).
    pub(super) fn new(fixed: u16, optional: u16, graph: bool) -> Self {
        let (min_graph, graph_rows) = if graph {
            (1, cards::GRAPH_ROWS)
        } else {
            (0, 0)
        };
        let min = (2 + fixed + min_graph).max(CARD_MIN_HEIGHT);
        let graphed = (2 + fixed + graph_rows).max(CARD_MIN_HEIGHT);
        Self {
            min,
            listed: min,
            graphed,
            desired: (graphed + optional).max(CARD_MIN_HEIGHT),
            graph,
        }
    }

    /// A card of `rows` list rows, which keep their room until the graphs
    /// are down to one row.
    pub(super) fn list(rows: u16) -> Self {
        let listed = (2 + rows).max(CARD_MIN_HEIGHT);
        Self {
            min: CARD_MIN_HEIGHT,
            listed,
            graphed: listed,
            desired: listed,
            graph: false,
        }
    }
}

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mode = WidthMode::for_width(area.width);
    let show_pinned = mode == WidthMode::Wide || app.pinned_processes().next().is_some();
    // The narrow column draws graphs above the rows, as wider modes do, once
    // every card fits whole that way.
    let graphs = mode != WidthMode::Narrow || {
        let inner_width = area.width.saturating_sub(2);
        narrow_cards(show_pinned)
            .map(|card| u32::from(card_height(app, card, inner_width, true).desired))
            .sum::<u32>()
            <= u32::from(area.height)
    };
    let placed = overview_layout(area, mode, show_pinned, |card, width| {
        card_height(app, card, width.saturating_sub(2), graphs)
    });
    for (card, rect) in placed {
        render_card(frame, app, card, rect, graphs);
    }
}

/// The narrow column's cards in priority order.
fn narrow_cards(show_pinned: bool) -> impl Iterator<Item = Card> {
    [
        Some(Card::Cpu),
        Some(Card::Memory),
        show_pinned.then_some(Card::Pinned),
        Some(Card::Network),
        Some(Card::Storage),
        Some(Card::Gpu),
    ]
    .into_iter()
    .flatten()
}

fn card_height(app: &App, card: Card, inner_width: u16, graphs: bool) -> CardHeight {
    match card {
        Card::Cpu => hardware_cpu::card_height(app.system_metrics(), inner_width, graphs),
        Card::Gpu => hardware::gpu_card_height(app, graphs),
        Card::Memory => hardware::memory_card_height(app, graphs),
        Card::Network => hardware_network_summary::card_height(app, graphs),
        Card::Storage => {
            hardware::storage_card_height(app.hardware(), app.system_metrics(), inner_width)
        }
        Card::Pinned => CardHeight::list(app.pinned_processes().count() as u16),
    }
}

fn render_card(frame: &mut Frame, app: &App, card: Card, area: Rect, graphs: bool) {
    match card {
        Card::Cpu => hardware_cpu::render_card(frame, app, area, graphs),
        Card::Gpu => hardware::render_gpu_card(frame, app, area, graphs),
        Card::Memory => hardware::render_memory_card(frame, app, area, graphs),
        Card::Network => hardware_network_summary::render_card(frame, app, area, graphs),
        Card::Storage => hardware::render_storage_card(frame, app, area),
        Card::Pinned => render_pinned_card(frame, app, area),
    }
}

/// Places the cards for `mode`. Rows get their minimum height in priority
/// order (top to bottom; in the narrow column CPU → Memory → Pinned →
/// Network → Storage → GPU); a row that does not fit is left out with every
/// row after it. When every row is shown, rows grow step by step (see
/// [`CardHeight`]), and what is left goes to the rows with graphs. Cards
/// side by side get the same width: an odd column goes
/// to the Pinned column or is left blank on the right.
pub(super) fn overview_layout(
    area: Rect,
    mode: WidthMode,
    show_pinned: bool,
    heights: impl Fn(Card, u16) -> CardHeight,
) -> Vec<(Card, Rect)> {
    let mut placed = Vec::new();
    let mut grid = area;
    let mut pinned_column = None;
    let rows: Vec<Vec<(Card, u16)>> = match mode {
        WidthMode::Wide => {
            let mut pinned_width = area.width / 4;
            // Even, so the grid's halves are equally wide and their graphs
            // span the same time.
            pinned_width += (area.width - pinned_width) % 2;
            grid.width -= pinned_width;
            if show_pinned && area.height >= CARD_MIN_HEIGHT && pinned_width > 0 {
                pinned_column = Some(pinned_width);
            }
            vec![
                halves(Card::Cpu, Card::Gpu, grid.width),
                halves(Card::Memory, Card::Network, grid.width),
                vec![(Card::Storage, grid.width)],
            ]
        }
        WidthMode::Medium => {
            grid.width -= area.width % 2;
            let bottom = if show_pinned {
                let pinned = grid.width / 3;
                vec![(Card::Storage, grid.width - pinned), (Card::Pinned, pinned)]
            } else {
                vec![(Card::Storage, grid.width)]
            };
            vec![
                halves(Card::Cpu, Card::Gpu, grid.width),
                halves(Card::Memory, Card::Network, grid.width),
                bottom,
            ]
        }
        WidthMode::Narrow => narrow_cards(show_pinned)
            .map(|card| vec![(card, area.width)])
            .collect(),
    };

    let row_heights: Vec<CardHeight> = rows
        .iter()
        .map(|row| {
            let cards: Vec<CardHeight> = row
                .iter()
                .map(|&(card, width)| heights(card, width))
                .collect();
            let tallest = |step: fn(&CardHeight) -> u16| cards.iter().map(step).max().unwrap_or(0);
            CardHeight {
                min: tallest(|height| height.min),
                listed: tallest(|height| height.listed),
                graphed: tallest(|height| height.graphed),
                desired: tallest(|height| height.desired),
                graph: cards.iter().any(|height| height.graph),
            }
        })
        .collect();
    let allocated = allocate_rows(grid.height, &row_heights);

    let mut y = grid.y;
    for (row, height) in rows.iter().zip(allocated) {
        if height == 0 {
            break;
        }
        let mut x = grid.x;
        for &(card, width) in row {
            placed.push((card, Rect::new(x, y, width, height)));
            x += width;
        }
        y += height;
    }
    if let Some(width) = pinned_column {
        // As tall as the cards beside it, or the whole height when none fit.
        let height = match y - grid.y {
            0 => area.height,
            used => used,
        };
        placed.insert(
            0,
            (Card::Pinned, Rect::new(grid.right(), area.y, width, height)),
        );
    }
    placed
}

fn halves(left: Card, right: Card, width: u16) -> Vec<(Card, u16)> {
    let right_width = width / 2;
    vec![(left, width - right_width), (right, right_width)]
}

/// Heights for `rows` within `total`: minimums in order (a row that does not
/// fit and every later one get 0); then, once every row is shown, growth in
/// order toward each step of [`CardHeight`] in turn, and the rest shared
/// evenly by the rows with graphs.
fn allocate_rows(total: u16, rows: &[CardHeight]) -> Vec<u16> {
    let mut heights = vec![0; rows.len()];
    let mut remaining = total;
    let mut shown = 0;
    for (height, row) in heights.iter_mut().zip(rows) {
        if row.min > remaining {
            break;
        }
        *height = row.min;
        remaining -= row.min;
        shown += 1;
    }
    // Rows grow only once every row is shown: growing an early row while a
    // later one is left out would take that growth back when the area gets
    // tall enough for the later row, and a taller terminal would show fewer
    // CPUs. Until then the spare rows stay blank.
    if shown < rows.len() {
        return heights;
    }
    let steps: [fn(&CardHeight) -> u16; 3] =
        [|row| row.listed, |row| row.graphed, |row| row.desired];
    for step in steps {
        for (height, row) in heights.iter_mut().zip(rows) {
            let addition = step(row).saturating_sub(*height).min(remaining);
            *height += addition;
            remaining -= addition;
        }
    }
    let growing = rows.iter().filter(|row| row.graph).count() as u16;
    if let Some(share) = remaining.checked_div(growing) {
        let mut extra = remaining % growing;
        for (height, _) in heights.iter_mut().zip(rows).filter(|(_, row)| row.graph) {
            *height += share + u16::from(extra > 0);
            extra = extra.saturating_sub(1);
        }
    }
    heights
}

/// `· host · kernel · up 2d 3h · 397 proc · 2 run · 0 zombie `, shortened to
/// `width`: the kernel goes first, then the counts become `397p · 2r · 0z`,
/// then the uptime goes. The host and the zombie count stay; `None` when not
/// even they fit.
pub(super) fn header_line(app: &App, width: usize) -> Option<Line<'static>> {
    let metrics = app.system_metrics();
    let host = metrics.system_identity.hostname.clone();
    let kernel = metrics.system_identity.kernel_release.clone();
    let uptime = metrics
        .uptime
        .map(|uptime| format!("up {}", format_uptime(uptime)));
    let summary = app.process_error().is_none().then(|| app.process_summary());
    let zombie_style = |zombies: usize| {
        Style::default().fg(if zombies == 0 {
            theme::MUTED
        } else {
            theme::WARNING
        })
    };
    let counts = |long: bool, all: bool| -> Vec<Span<'static>> {
        let Some(summary) = summary else {
            return Vec::new();
        };
        let (proc, run, zombie) = if long {
            (" proc", " run", " zombie")
        } else {
            ("p", "r", "z")
        };
        let mut spans = Vec::new();
        if all {
            spans.push(Span::raw(format!("{}{proc}", summary.total)));
            spans.push(Span::raw(format!("{}{run}", summary.running)));
        }
        spans.push(Span::styled(
            format!("{}{zombie}", summary.zombies),
            zombie_style(summary.zombies),
        ));
        spans
    };
    let text = |value: &Option<String>| value.clone().map(Span::raw);
    let variants: [Vec<Option<Span<'static>>>; 5] = [
        [text(&host), text(&kernel), text(&uptime)]
            .into_iter()
            .chain(counts(true, true).into_iter().map(Some))
            .collect(),
        [text(&host), text(&uptime)]
            .into_iter()
            .chain(counts(true, true).into_iter().map(Some))
            .collect(),
        [text(&host), text(&uptime)]
            .into_iter()
            .chain(counts(false, true).into_iter().map(Some))
            .collect(),
        [text(&host)]
            .into_iter()
            .chain(counts(false, true).into_iter().map(Some))
            .collect(),
        [text(&host)]
            .into_iter()
            .chain(counts(false, false).into_iter().map(Some))
            .collect(),
    ];
    variants.into_iter().find_map(|parts| {
        let parts: Vec<Span<'static>> = parts.into_iter().flatten().collect();
        if parts.is_empty() {
            return None;
        }
        let mut spans = Vec::with_capacity(parts.len() * 2 + 1);
        for part in parts {
            spans.push(Span::styled("· ", Style::default().fg(theme::MUTED)));
            spans.push(part);
            spans.push(Span::raw(" "));
        }
        let line = Line::from(spans);
        (line.width() <= width).then_some(line)
    })
}

fn render_pinned_card(frame: &mut Frame, app: &App, area: Rect) {
    let inner = cards::padded(cards::render_card(
        frame,
        area,
        &CardTitle::plain("Pinned"),
        None,
    ));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = pinned_lines(app, usize::from(inner.width), usize::from(inner.height));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row per pinned process (name, CPU and memory), with `… N more` when
/// they do not all fit; a hint naming the pin key when there are none.
fn pinned_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    // ` 1200.0%  64.0 MiB`: CPU is top-style, up to 100 % per core.
    const CPU_WIDTH: usize = 7;
    const VALUES_WIDTH: usize = 1 + CPU_WIDTH + 1 + 9;
    let pins: Vec<_> = app.pinned_processes().collect();
    if height == 0 {
        return Vec::new();
    }
    if pins.is_empty() {
        let hint = keymap::key_for(keymap::PROCESSES, Action::TogglePin).map_or_else(
            || "No pinned processes".to_owned(),
            |key| format!("{key} on Processes pins a process"),
        );
        return vec![
            Line::from(layout::truncate(&hint, width)).style(Style::default().fg(theme::MUTED))
        ];
    }
    let overflow = pins.len() > height;
    let shown = if overflow { height - 1 } else { pins.len() };
    let name_width = width.saturating_sub(VALUES_WIDTH).max(1);
    let mut lines: Vec<Line<'static>> = pins
        .iter()
        .take(shown)
        .map(|pin| {
            let process = pin.process;
            let name = layout::truncate(&process.name, name_width);
            if pin.exited {
                Line::from(layout::truncate(
                    &format!("{name:<name_width$} exited"),
                    width,
                ))
                .style(Style::default().fg(theme::MUTED))
            } else if width > VALUES_WIDTH + 4 {
                Line::from(vec![
                    Span::raw(format!("{name:<name_width$} ")),
                    pinned_cpu(process.cpu_percent),
                    Span::raw(format!(" {:>9}", format_bytes(process.memory_bytes))),
                ])
            } else {
                let name = layout::truncate(&process.name, width.saturating_sub(CPU_WIDTH + 1));
                let cpu = pinned_cpu(process.cpu_percent);
                if name.is_empty() || width < CPU_WIDTH + 2 {
                    Line::from(layout::truncate(&name, width))
                } else {
                    Line::from(vec![Span::raw(format!("{name} ")), cpu])
                }
            }
        })
        .collect();
    if overflow {
        lines.push(Line::from(format!("… {} more", pins.len() - shown)));
    }
    lines
}

/// `  12.5%`, colored by its band; a process busy on several cores is
/// placed at 100 %.
fn pinned_cpu(percent: Option<f64>) -> Span<'static> {
    match percent {
        Some(percent) => Span::styled(
            format!("{percent:>6.1}%"),
            Style::default().fg(theme::band(percent.min(100.0))),
        ),
        None => Span::styled(format!("{:>7}", "N/A"), Style::default().fg(theme::MUTED)),
    }
}

#[cfg(test)]
mod tests;
