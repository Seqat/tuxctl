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
    format_bytes, format_uptime, hardware, hardware_cpu, hardware_network_summary, layout,
    processes, theme,
};

/// Content widths (the terminal is two columns wider) at which the Overview
/// switches from one column to a 2×2 grid, and adds a Pinned column.
pub(super) const MEDIUM_MIN_WIDTH: u16 = 98;
pub(super) const WIDE_MIN_WIDTH: u16 = 148;
/// A card is a border plus at least one row.
pub(super) const CARD_MIN_HEIGHT: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WidthMode {
    /// One column; graphs sit in the card titles.
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

    /// Wide and medium cards draw their graph above their rows.
    pub(super) fn graphs(self) -> bool {
        self != Self::Narrow
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

/// Heights a card needs, borders included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CardHeight {
    pub min: u16,
    pub desired: u16,
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
        Self {
            min: (2 + fixed + min_graph).max(CARD_MIN_HEIGHT),
            desired: (2 + fixed + optional + graph_rows).max(CARD_MIN_HEIGHT),
        }
    }
}

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mode = WidthMode::for_width(area.width);
    let show_pinned = mode == WidthMode::Wide || app.pinned_processes().next().is_some();
    let placed = overview_layout(area, mode, show_pinned, |card, width| {
        card_height(app, card, width.saturating_sub(2), mode)
    });
    for (card, rect) in placed {
        render_card(frame, app, card, rect, mode);
    }
}

fn card_height(app: &App, card: Card, inner_width: u16, mode: WidthMode) -> CardHeight {
    let graphs = mode.graphs();
    match card {
        Card::Cpu => hardware_cpu::card_height(app.system_metrics(), inner_width, graphs),
        Card::Gpu => hardware::gpu_card_height(app.hardware()),
        Card::Memory => hardware::memory_card_height(app, graphs),
        Card::Network => hardware_network_summary::card_height(app, graphs),
        Card::Storage => {
            hardware::storage_card_height(app.hardware(), app.system_metrics(), inner_width)
        }
        Card::Pinned => CardHeight::new(
            1,
            (app.pinned_processes().count() as u16).saturating_sub(1),
            false,
        ),
    }
}

fn render_card(frame: &mut Frame, app: &App, card: Card, area: Rect, mode: WidthMode) {
    let graphs = mode.graphs();
    match card {
        Card::Cpu => hardware_cpu::render_card(frame, app, area, graphs),
        Card::Gpu => hardware::render_gpu_card(frame, app, area),
        Card::Memory => hardware::render_memory_card(frame, app, area, graphs),
        Card::Network => hardware_network_summary::render_card(frame, app, area, graphs),
        Card::Storage => hardware::render_storage_card(frame, app, area),
        Card::Pinned => render_pinned_card(frame, app, area),
    }
}

/// Places the cards for `mode`. Rows get their minimum height in priority
/// order (top to bottom; in the narrow column CPU → Memory → Pinned →
/// Network → Storage → GPU); a row that does not fit is left out with every
/// row after it. When every row is shown, rows grow toward their desired
/// height, and what is left goes to the rows with graphs.
pub(super) fn overview_layout(
    area: Rect,
    mode: WidthMode,
    show_pinned: bool,
    heights: impl Fn(Card, u16) -> CardHeight,
) -> Vec<(Card, Rect)> {
    let mut placed = Vec::new();
    let mut grid = area;
    let rows: Vec<Vec<(Card, u16)>> = match mode {
        WidthMode::Wide => {
            let pinned_width = area.width / 4;
            grid.width -= pinned_width;
            if show_pinned && area.height >= CARD_MIN_HEIGHT && pinned_width > 0 {
                placed.push((
                    Card::Pinned,
                    Rect::new(grid.right(), area.y, pinned_width, area.height),
                ));
            }
            vec![
                halves(Card::Cpu, Card::Gpu, grid.width),
                halves(Card::Memory, Card::Network, grid.width),
                vec![(Card::Storage, grid.width)],
            ]
        }
        WidthMode::Medium => {
            let bottom = if show_pinned {
                let pinned = area.width / 3;
                vec![(Card::Storage, area.width - pinned), (Card::Pinned, pinned)]
            } else {
                vec![(Card::Storage, area.width)]
            };
            vec![
                halves(Card::Cpu, Card::Gpu, area.width),
                halves(Card::Memory, Card::Network, area.width),
                bottom,
            ]
        }
        WidthMode::Narrow => [
            Some(Card::Cpu),
            Some(Card::Memory),
            show_pinned.then_some(Card::Pinned),
            Some(Card::Network),
            Some(Card::Storage),
            Some(Card::Gpu),
        ]
        .into_iter()
        .flatten()
        .map(|card| vec![(card, area.width)])
        .collect(),
    };

    let row_heights: Vec<CardHeight> = rows
        .iter()
        .map(|row| CardHeight {
            min: row
                .iter()
                .map(|&(card, width)| heights(card, width).min)
                .max()
                .unwrap_or(0),
            desired: row
                .iter()
                .map(|&(card, width)| heights(card, width).desired)
                .max()
                .unwrap_or(0),
        })
        .collect();
    let allocated = allocate_rows(grid.height, &row_heights, |index| {
        mode.graphs() && index < 2
    });

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
    placed
}

fn halves(left: Card, right: Card, width: u16) -> Vec<(Card, u16)> {
    let right_width = width / 2;
    vec![(left, width - right_width), (right, right_width)]
}

/// Heights for `rows` within `total`: minimums in order (a row that does not
/// fit and every later one get 0); then, once every row is shown, growth
/// toward the desired heights in order and the rest shared evenly by the rows
/// `grows` selects.
fn allocate_rows(total: u16, rows: &[CardHeight], grows: impl Fn(usize) -> bool) -> Vec<u16> {
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
    for (height, row) in heights.iter_mut().zip(rows) {
        let addition = row.desired.saturating_sub(*height).min(remaining);
        *height += addition;
        remaining -= addition;
    }
    let growing: Vec<usize> = (0..shown).filter(|&index| grows(index)).collect();
    if !growing.is_empty() {
        let share = remaining / growing.len() as u16;
        let mut extra = remaining % growing.len() as u16;
        for index in growing {
            heights[index] += share + u16::from(extra > 0);
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

/// One row per pinned process, with `… N more` when they do not all fit; a
/// hint naming the pin key when there are none.
fn pinned_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    const VALUES_WIDTH: usize = 17;
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
                Line::from(format!(
                    "{name:<name_width$} {:>6} {:>9}",
                    processes::format_cpu(process.cpu_percent),
                    format_bytes(process.memory_bytes)
                ))
            } else {
                Line::from(layout::truncate(
                    &format!("{name} {}", processes::format_cpu(process.cpu_percent)),
                    width,
                ))
            }
        })
        .collect();
    if overflow {
        lines.push(Line::from(format!("… {} more", pins.len() - shown)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(min: u16, desired: u16) -> impl Fn(Card, u16) -> CardHeight {
        move |_, _| CardHeight { min, desired }
    }

    fn cards(placed: &[(Card, Rect)]) -> Vec<Card> {
        placed.iter().map(|(card, _)| *card).collect()
    }

    #[test]
    fn width_modes_switch_at_their_boundaries() {
        assert_eq!(
            WidthMode::for_width(MEDIUM_MIN_WIDTH - 1),
            WidthMode::Narrow
        );
        assert_eq!(WidthMode::for_width(MEDIUM_MIN_WIDTH), WidthMode::Medium);
        assert_eq!(WidthMode::for_width(WIDE_MIN_WIDTH - 1), WidthMode::Medium);
        assert_eq!(WidthMode::for_width(WIDE_MIN_WIDTH), WidthMode::Wide);
        // The terminal is two columns wider than the Overview.
        assert_eq!(MEDIUM_MIN_WIDTH + 2, 100);
        assert_eq!(WIDE_MIN_WIDTH + 2, 150);
    }

    #[test]
    fn wide_places_a_grid_storage_and_a_pinned_column() {
        let area = Rect::new(1, 2, 200, 50);
        let placed = overview_layout(area, WidthMode::Wide, true, fixed(4, 8));
        assert_eq!(
            cards(&placed),
            [
                Card::Pinned,
                Card::Cpu,
                Card::Gpu,
                Card::Memory,
                Card::Network,
                Card::Storage
            ]
        );
        let rect = |card| placed.iter().find(|(c, _)| *c == card).unwrap().1;
        assert_eq!(rect(Card::Pinned), Rect::new(151, 2, 50, 50));
        assert_eq!(rect(Card::Cpu).x, 1);
        assert_eq!(rect(Card::Gpu).right(), 151);
        assert_eq!(rect(Card::Storage).width, 150);
        assert_eq!(rect(Card::Storage).bottom(), 2 + 8 + 8 + 8 + (50 - 24));
        assert!(rect(Card::Cpu).height > 8, "graph rows take what is left");
    }

    #[test]
    fn medium_puts_storage_and_pinned_side_by_side_or_storage_alone() {
        let area = Rect::new(0, 0, 120, 40);
        let with = overview_layout(area, WidthMode::Medium, true, fixed(4, 6));
        let rect =
            |placed: &[(Card, Rect)], card| placed.iter().find(|(c, _)| *c == card).unwrap().1;
        assert_eq!(rect(&with, Card::Storage).width, 80);
        assert_eq!(rect(&with, Card::Pinned).width, 40);
        let without = overview_layout(area, WidthMode::Medium, false, fixed(4, 6));
        assert!(!cards(&without).contains(&Card::Pinned));
        assert_eq!(rect(&without, Card::Storage).width, 120);
    }

    #[test]
    fn narrow_stacks_by_priority_and_leaves_out_what_does_not_fit() {
        let area = Rect::new(0, 0, 60, 100);
        let all = overview_layout(area, WidthMode::Narrow, true, fixed(3, 5));
        assert_eq!(
            cards(&all),
            [
                Card::Cpu,
                Card::Memory,
                Card::Pinned,
                Card::Network,
                Card::Storage,
                Card::Gpu
            ]
        );
        let short = overview_layout(
            Rect::new(0, 0, 60, 13),
            WidthMode::Narrow,
            false,
            fixed(3, 5),
        );
        assert_eq!(
            cards(&short),
            [Card::Cpu, Card::Memory, Card::Network, Card::Storage]
        );
    }

    #[test]
    fn cards_never_overlap_or_leave_the_area() {
        for mode in [WidthMode::Narrow, WidthMode::Medium, WidthMode::Wide] {
            for width in 0..=220 {
                for height in 0..=70 {
                    let area = Rect::new(3, 4, width, height);
                    let placed = overview_layout(area, mode, true, |card, width| CardHeight {
                        min: 3 + (card as u16) % 3,
                        desired: 6 + width % 5,
                    });
                    for (index, (_, rect)) in placed.iter().enumerate() {
                        assert_eq!(rect.intersection(area), *rect, "{mode:?} {width}x{height}");
                        assert!(rect.height >= CARD_MIN_HEIGHT, "{mode:?} {width}x{height}");
                        for (_, other) in &placed[index + 1..] {
                            assert!(!rect.intersects(*other), "{mode:?} {width}x{height}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rows_get_minimums_then_desired_heights_then_graph_room() {
        let rows = [
            CardHeight {
                min: 4,
                desired: 10,
            },
            CardHeight { min: 4, desired: 6 },
            CardHeight { min: 3, desired: 5 },
        ];
        let graphs = |index: usize| index < 2;
        // Only the first minimum fits; nothing grows while a row is left out.
        assert_eq!(allocate_rows(7, &rows, graphs), [4, 0, 0]);
        assert_eq!(allocate_rows(11, &rows, graphs), [4, 4, 3]);
        assert_eq!(allocate_rows(21, &rows, graphs), [10, 6, 5]);
        assert_eq!(allocate_rows(25, &rows, graphs), [12, 8, 5]);
        assert_eq!(allocate_rows(0, &rows, graphs), [0, 0, 0]);
    }

    fn process(pid: u32, state_code: char) -> crate::linux::ProcessInfo {
        crate::linux::ProcessInfo {
            pid,
            name: format!("p{pid}"),
            cpu_percent: None,
            memory_bytes: 0,
            command: None,
            state: String::new(),
            parent_pid: 1,
            state_code,
            start_time: u64::from(pid),
            kernel_thread: false,
        }
    }

    fn header_app(zombies: u32) -> App {
        let mut app = App::default();
        let mut metrics = crate::linux::SystemMetrics {
            uptime: Some(std::time::Duration::from_secs(
                2 * 86_400 + 13 * 3600 + 58 * 60,
            )),
            ..crate::linux::SystemMetrics::default()
        };
        metrics.system_identity.hostname = Some("Monolith".into());
        metrics.system_identity.kernel_release = Some("7.2.6-1-cachyos".into());
        app.update(Action::SystemMetricsUpdated(metrics));
        let mut processes: Vec<_> = (1..=397).map(|pid| process(pid, 'S')).collect();
        processes[0].state_code = 'R';
        processes[1].state_code = 'R';
        for index in 0..zombies as usize {
            processes[10 + index].state_code = 'Z';
        }
        app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes,
            error: None,
        }));
        app
    }

    fn header_text(app: &App, width: usize) -> Option<String> {
        header_line(app, width).map(|line| line.to_string())
    }

    #[test]
    fn the_header_drops_kernel_then_abbreviates_then_drops_uptime() {
        let app = header_app(0);
        let full = "· Monolith · 7.2.6-1-cachyos · up 2d 13h 58m · 397 proc · 2 run · 0 zombie ";
        let steps = [
            full,
            "· Monolith · up 2d 13h 58m · 397 proc · 2 run · 0 zombie ",
            "· Monolith · up 2d 13h 58m · 397p · 2r · 0z ",
            "· Monolith · 397p · 2r · 0z ",
            "· Monolith · 0z ",
        ];
        let mut seen = Vec::new();
        for width in (0..=100).rev() {
            let text = header_text(&app, width);
            if let Some(text) = &text {
                assert!(text.chars().count() <= width, "{width}: {text}");
            }
            if seen.last() != Some(&text) {
                seen.push(text);
            }
        }
        let expected: Vec<Option<String>> = steps
            .iter()
            .map(|step| Some((*step).to_owned()))
            .chain([None])
            .collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn zombies_are_muted_when_zero_and_a_warning_otherwise() {
        let zombie_style = |app: &App| {
            header_line(app, 200)
                .unwrap()
                .spans
                .iter()
                .find(|span| span.content.contains("zombie"))
                .unwrap()
                .style
                .fg
        };
        assert_eq!(zombie_style(&header_app(0)), Some(theme::MUTED));
        assert_eq!(zombie_style(&header_app(3)), Some(theme::WARNING));
    }

    #[test]
    fn process_counts_leave_the_header_while_process_data_is_unavailable() {
        let mut app = header_app(1);
        assert!(header_text(&app, 200).unwrap().contains("1 zombie"));
        app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes: Vec::new(),
            error: Some("proc unavailable".into()),
        }));
        let stale = header_text(&app, 200).unwrap();
        assert!(
            !stale.contains("proc") && !stale.contains("zombie"),
            "{stale}"
        );
        assert!(stale.contains("Monolith"));
    }

    fn snapshot(entries: &[(u32, &str, f64)]) -> Action {
        Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes: entries
                .iter()
                .map(|&(pid, name, cpu)| crate::linux::ProcessInfo {
                    name: name.into(),
                    cpu_percent: Some(cpu),
                    memory_bytes: 64 << 20,
                    ..process(pid, 'S')
                })
                .collect(),
            error: None,
        })
    }

    /// Pins `pids` on the Processes tab, then returns to the Overview.
    fn overview_with_pins(entries: &[(u32, &str, f64)], pids: &[u32]) -> App {
        let mut app = App::default();
        app.update(Action::SelectTab(crate::action::Tab::Processes));
        app.update(snapshot(entries));
        for &pid in pids {
            app.update(Action::SelectProcess(crate::linux::ProcessIdentity {
                pid,
                start_time: u64::from(pid),
            }));
            app.update(Action::TogglePin);
        }
        app.update(Action::SelectTab(crate::action::Tab::Overview));
        app
    }

    fn pinned_card(app: &App, width: u16, height: u16) -> (Vec<String>, ratatui::buffer::Buffer) {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_pinned_card(frame, app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        (rows, buffer)
    }

    #[test]
    fn pinned_processes_are_listed_in_pin_order_with_cpu_and_memory() {
        let app = overview_with_pins(
            &[
                (1, "postgres", 12.5),
                (2, "firefox", 30.0),
                (3, "bash", 0.0),
            ],
            &[2, 1],
        );
        let (rows, _) = pinned_card(&app, 40, 6);
        assert!(rows[0].contains(" Pinned "));
        assert!(
            rows[1].contains("firefox") && rows[1].contains("30.0%"),
            "{rows:#?}"
        );
        assert!(rows[1].contains("64.0 MiB"), "{rows:#?}");
        assert!(rows[2].contains("postgres"), "pin order: {rows:#?}");
        assert!(!rows.iter().any(|row| row.contains("bash")));
    }

    #[test]
    fn exited_pins_are_shown_dimmed() {
        let mut app = overview_with_pins(&[(1, "worker", 5.0), (2, "bash", 1.0)], &[1]);
        app.update(snapshot(&[(2, "bash", 1.0)]));
        let (rows, buffer) = pinned_card(&app, 40, 5);
        assert!(
            rows[1].contains("worker") && rows[1].contains("exited"),
            "{rows:#?}"
        );
        // The name starts a column in from the border.
        assert_eq!(buffer[(2, 1)].fg, theme::MUTED);
    }

    #[test]
    fn pins_that_do_not_fit_are_counted() {
        let names: Vec<String> = (1..=8).map(|pid| format!("proc{pid}")).collect();
        let entries: Vec<(u32, &str, f64)> = names
            .iter()
            .enumerate()
            .map(|(index, name)| (index as u32 + 1, name.as_str(), 1.0))
            .collect();
        let app = overview_with_pins(&entries, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let (rows, _) = pinned_card(&app, 40, 6);
        assert!(rows[4].contains("… 5 more"), "{rows:#?}");
    }

    #[test]
    fn without_pins_wide_shows_the_pin_key_and_narrower_modes_hide_the_card() {
        let app = overview_with_pins(&[(1, "a", 1.0)], &[]);
        let (rows, _) = pinned_card(&app, 40, 4);
        assert!(
            rows[1].contains("P on Processes pins a process"),
            "{rows:#?}"
        );

        let placed = |width: u16| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 40)).unwrap();
            terminal
                .draw(|frame| render(frame, &app, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..40)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .any(|row| row.contains(" Pinned "))
        };
        assert!(placed(WIDE_MIN_WIDTH));
        assert!(!placed(WIDE_MIN_WIDTH - 1));
        assert!(!placed(60));
    }
}
