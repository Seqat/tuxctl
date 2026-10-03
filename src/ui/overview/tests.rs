use super::*;

/// A height that grows from `min` to `desired` in its last step, and
/// further with spare rows when it has a `graph`.
fn staged(min: u16, desired: u16, graph: bool) -> CardHeight {
    CardHeight {
        min,
        listed: min,
        graphed: min,
        desired,
        graph,
    }
}

/// Every card but Storage and Pinned has a graph.
fn fixed(min: u16, desired: u16) -> impl Fn(Card, u16) -> CardHeight {
    move |card, _| staged(min, desired, !matches!(card, Card::Storage | Card::Pinned))
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
    let rect = |placed: &[(Card, Rect)], card| placed.iter().find(|(c, _)| *c == card).unwrap().1;
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
                let placed = overview_layout(area, mode, true, |card, width| {
                    staged(3 + (card as u16) % 3, 6 + width % 5, width % 2 == 0)
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
fn side_by_side_cards_are_equally_wide_on_odd_widths() {
    let width_of = |placed: &[(Card, Rect)], card| {
        placed
            .iter()
            .find(|(c, _)| *c == card)
            .map(|(_, rect)| rect.width)
            .unwrap()
    };
    for width in [149, 150, 151, 201] {
        let area = Rect::new(0, 0, width, 40);
        let wide = overview_layout(area, WidthMode::Wide, true, fixed(4, 8));
        assert_eq!(
            width_of(&wide, Card::Cpu),
            width_of(&wide, Card::Gpu),
            "{width}"
        );
        let right = wide.iter().map(|(_, rect)| rect.right()).max();
        assert_eq!(right, Some(width), "the Pinned column takes the odd column");
    }
    for width in [98, 99, 120, 147] {
        let area = Rect::new(0, 0, width, 40);
        let medium = overview_layout(area, WidthMode::Medium, true, fixed(4, 8));
        assert_eq!(
            width_of(&medium, Card::Memory),
            width_of(&medium, Card::Network),
            "{width}"
        );
        let right = medium.iter().map(|(_, rect)| rect.right()).max().unwrap();
        assert_eq!(right, width - width % 2, "{width}");
    }
}

#[test]
fn spare_rows_go_to_the_graphs_and_fill_the_overview() {
    let heights = |card: Card, _| match card {
        Card::Storage | Card::Pinned => CardHeight::list(2),
        _ => CardHeight::new(1, 0, true),
    };
    // 7 rows per grid row and 4 for Storage; the 62 left are shared by
    // the two grid rows.
    let placed = overview_layout(Rect::new(0, 0, 200, 80), WidthMode::Wide, true, heights);
    let rect = |card| placed.iter().find(|(c, _)| *c == card).unwrap().1;
    assert_eq!(rect(Card::Cpu).height, 7 + 31);
    assert_eq!(rect(Card::Storage).height, 4);
    assert_eq!(rect(Card::Storage).bottom(), 80);
    assert_eq!(rect(Card::Pinned).height, 80);
}

#[test]
fn rows_get_minimums_then_desired_heights_then_graph_room() {
    let rows = [staged(4, 10, true), staged(4, 6, true), staged(3, 5, false)];
    // Only the first minimum fits; nothing grows while a row is left out.
    assert_eq!(allocate_rows(7, &rows), [4, 0, 0]);
    assert_eq!(allocate_rows(11, &rows), [4, 4, 3]);
    assert_eq!(allocate_rows(21, &rows), [10, 6, 5]);
    assert_eq!(allocate_rows(25, &rows), [12, 8, 5]);
    assert_eq!(allocate_rows(40, &rows), [20, 15, 5]);
    assert_eq!(allocate_rows(0, &rows), [0, 0, 0]);
}

#[test]
fn a_shrinking_overview_drops_optional_rows_then_graph_rows_then_list_rows() {
    // CPU (a 4-row grid) | GPU, Memory | Network (2 more interfaces),
    // Storage | Pinned (6 pins): 11 rows at their minimum, 16 with whole
    // lists, 22 with full graphs, 28 with everything.
    let rows = [
        CardHeight::new(1, 4, true),
        CardHeight::new(1, 2, true),
        CardHeight::list(6),
    ];
    assert_eq!(
        rows[0],
        CardHeight {
            min: 4,
            listed: 4,
            graphed: 7,
            desired: 11,
            graph: true
        }
    );
    assert_eq!(
        rows[2],
        CardHeight {
            min: 3,
            listed: 8,
            graphed: 8,
            desired: 8,
            graph: false
        }
    );
    assert_eq!(allocate_rows(40, &rows), [17, 15, 8]);
    assert_eq!(allocate_rows(28, &rows), [11, 9, 8]);
    // The optional rows go first, those of the lower row first.
    assert_eq!(allocate_rows(26, &rows), [11, 7, 8]);
    assert_eq!(allocate_rows(22, &rows), [7, 7, 8]);
    // Then the graphs shrink to one row.
    assert_eq!(allocate_rows(19, &rows), [7, 4, 8]);
    assert_eq!(allocate_rows(16, &rows), [4, 4, 8]);
    // Then the lists, down to one row (and `… N more`).
    assert_eq!(allocate_rows(15, &rows), [4, 4, 7]);
    assert_eq!(allocate_rows(11, &rows), [4, 4, 3]);
    // Then whole rows, from the last.
    assert_eq!(allocate_rows(10, &rows), [4, 4, 0]);
    assert_eq!(allocate_rows(7, &rows), [4, 0, 0]);

    // No row gets shorter as the area grows.
    let mut previous = vec![0; rows.len()];
    for total in 0..60 {
        let heights = allocate_rows(total, &rows);
        for (height, before) in heights.iter().zip(&previous) {
            assert!(height >= before, "{total}: {heights:?}");
        }
        previous = heights;
    }
}

#[test]
fn narrow_drops_the_gpu_card_only_after_pinned_is_down_to_one_row() {
    let heights = |card: Card, _| match card {
        Card::Pinned => CardHeight::list(5),
        _ => CardHeight::new(1, 0, false),
    };
    let narrow = |height| {
        let placed = overview_layout(
            Rect::new(0, 0, 60, height),
            WidthMode::Narrow,
            true,
            heights,
        );
        let pinned = placed
            .iter()
            .find(|(card, _)| *card == Card::Pinned)
            .map(|(_, rect)| rect.height);
        (placed.last().map(|(card, _)| *card), pinned)
    };
    // Six cards of 3 rows; Pinned grows to 7 once all are shown.
    assert_eq!(narrow(22), (Some(Card::Gpu), Some(7)));
    assert_eq!(narrow(18), (Some(Card::Gpu), Some(3)));
    assert_eq!(narrow(17), (Some(Card::Storage), Some(3)));
}

#[test]
fn a_tall_narrow_overview_draws_graphs_inside_the_cards() {
    let mut app = App::default();
    for _ in 0..3 {
        app.update(Action::SystemMetricsUpdated(crate::linux::SystemMetrics {
            cpu_percent: Some(50.0),
            ..crate::linux::SystemMetrics::default()
        }));
    }
    let screen = |height: u16| {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect())
            .collect::<Vec<String>>()
    };
    let graph = |row: &String| row.contains('▄') || row.contains('█');

    let short = screen(15);
    assert!(
        short[0].starts_with("┌ CPU") && graph(&short[0]),
        "{short:#?}"
    );
    let tall = screen(60);
    assert!(!graph(&tall[0]), "{tall:#?}");
    assert!(tall[1..10].iter().any(graph), "{tall:#?}");
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
fn pinned_cpu_is_colored_by_band_and_a_multi_core_process_counts_as_full() {
    let app = overview_with_pins(&[(1, "idle", 2.0), (2, "build", 250.0)], &[1, 2]);
    let (rows, buffer) = pinned_card(&app, 40, 5);
    let cell_of = |row: usize, text: &str| {
        let x = rows[row].find(text).unwrap();
        let x = rows[row][..x].chars().count() as u16;
        buffer[(x, row as u16)].fg
    };
    assert!(rows[2].contains(" 250.0%"), "{rows:#?}");
    assert_eq!(cell_of(1, "2.0%"), theme::band(2.0));
    assert_eq!(cell_of(2, "250.0%"), theme::band(100.0));
    // The values line up whatever the width of the CPU value.
    assert_eq!(rows[1].find("MiB"), rows[2].find("MiB"), "{rows:#?}");
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
