use std::sync::Arc;

use crossterm::event::MouseEvent;
use ratatui::layout::Rect;

use super::*;
use crate::action::{IntervalStep, PinMove, ProcessSortField, Tab};
use crate::linux::ProcessIdentity;

fn no_regions() -> UiRegions {
    UiRegions::default()
}

const TAB_MODES: [InputMode; 4] = [
    InputMode::Normal,
    InputMode::Services,
    InputMode::Logs,
    InputMode::Network,
];

/// Every printable ASCII key (plain and shifted) plus the special keys.
fn candidate_keys() -> Vec<KeyEvent> {
    let mut keys: Vec<KeyEvent> = (0x20_u8..0x7f)
        .flat_map(|byte| {
            let code = KeyCode::Char(char::from(byte));
            [
                KeyEvent::new(code, KeyModifiers::NONE),
                KeyEvent::new(code, KeyModifiers::SHIFT),
            ]
        })
        .collect();
    for code in [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Insert,
    ] {
        keys.push(KeyEvent::new(code, KeyModifiers::NONE));
        keys.push(KeyEvent::new(code, KeyModifiers::SHIFT));
        keys.push(KeyEvent::new(code, KeyModifiers::ALT));
    }
    keys
}

#[test]
fn screen_keys_never_shadow_global_tab_keys() {
    type ScreenKeys = fn(KeyEvent) -> Option<Action>;
    let screens: [(&str, ScreenKeys); 4] = [
        ("processes", |key| keymap::find(keymap::PROCESSES, key)),
        ("services", |key| keymap::find(keymap::SERVICES, key)),
        ("logs", |key| keymap::find(keymap::LOGS, key)),
        ("network", |key| keymap::find(keymap::NETWORK, key)),
    ];
    for key in candidate_keys() {
        if keymap::find(keymap::GLOBAL, key).is_none() {
            continue;
        }
        for (screen, screen_key) in screens {
            assert_eq!(
                screen_key(key),
                None,
                "{screen} binds {key:?}, which is already a global key"
            );
        }
    }
}

#[test]
fn global_keys_behave_the_same_in_every_tab_mode() {
    for key in candidate_keys() {
        let Some(global) = keymap::find(keymap::GLOBAL, key) else {
            continue;
        };
        for mode in TAB_MODES {
            assert_eq!(
                translate_key_event(key, mode),
                Some(global.clone()),
                "{key:?} in {mode:?}"
            );
        }
    }
}

/// Maps a key as written in the docs/controls.md tables to a key event.
fn doc_key(token: &str) -> KeyEvent {
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    match token {
        "Tab" => plain(KeyCode::Tab),
        "Shift+Tab" => plain(KeyCode::BackTab),
        "→" => plain(KeyCode::Right),
        "←" => plain(KeyCode::Left),
        "↑" => plain(KeyCode::Up),
        "↓" => plain(KeyCode::Down),
        "PageUp" => plain(KeyCode::PageUp),
        "PageDown" => plain(KeyCode::PageDown),
        "Home" => plain(KeyCode::Home),
        "End" => plain(KeyCode::End),
        "Enter" => plain(KeyCode::Enter),
        "Esc" => plain(KeyCode::Esc),
        "Space" => plain(KeyCode::Char(' ')),
        "Ctrl+C" => KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        "Shift+K" => KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT),
        "Shift+T" => KeyEvent::new(KeyCode::Char('t'), KeyModifiers::SHIFT),
        "Shift+P" => KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT),
        "Shift+↑" => KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
        "Shift+↓" => KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
        "Alt+↑" => KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
        "Alt+↓" => KeyEvent::new(KeyCode::Down, KeyModifiers::ALT),
        single if single.chars().count() == 1 => {
            plain(KeyCode::Char(single.chars().next().unwrap()))
        }
        other => panic!("documented key {other:?} has no mapping in doc_key()"),
    }
}

#[test]
fn every_key_in_the_documented_tables_is_handled() {
    let page = include_str!("../../docs/controls.md");
    let start = page
        .find("## Global Controls")
        .expect("docs/controls.md key tables");
    let controls = page[start..]
        .split("## Mouse Controls")
        .next()
        .unwrap_or_default();
    let mut modes: &[InputMode] = &[];
    let mut checked = 0;
    for line in controls.lines() {
        if let Some(heading) = line.trim_start_matches('#').strip_prefix(' ') {
            if line.starts_with('#') {
                modes = match heading {
                    "Global Controls" | "Navigation & Common Actions" => &TAB_MODES,
                    "Processes" => &[InputMode::Normal],
                    "Signal Confirmation" => &[InputMode::ProcessSignalConfirm],
                    "Main Menu" => &[InputMode::Menu],
                    "Services" => &[InputMode::Services],
                    "Logs" => &[InputMode::Logs],
                    other => panic!("unknown controls section {other:?} in docs/controls.md"),
                };
                continue;
            }
        }
        let Some(keys) = line.strip_prefix("| `") else {
            continue;
        };
        let first_column = keys.split(" | ").next().unwrap_or_default();
        for token in format!("`{first_column}").split('`').skip(1).step_by(2) {
            let key = doc_key(token);
            for &mode in modes {
                // The Network screen has no search; the docs list `/` as common.
                if token == "/" && mode == InputMode::Network {
                    continue;
                }
                assert!(
                    translate_key_event(key, mode).is_some(),
                    "docs/controls.md documents `{token}` but {mode:?} ignores it"
                );
            }
            checked += 1;
        }
    }
    assert!(checked >= 30, "only {checked} documented keys were checked");
}

#[test]
fn number_keys_select_tabs() {
    let event = Event::Key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE));

    assert_eq!(
        translate_event(event, &no_regions(), None),
        Some(Action::SelectTab(Tab::Logs))
    );
}

#[test]
fn shift_tab_moves_to_previous_tab() {
    let event = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));

    assert_eq!(
        translate_event(event, &no_regions(), None),
        Some(Action::PreviousTab)
    );
}

#[test]
fn left_click_on_tab_selects_it() {
    let regions = UiRegions::from_tabs([(Tab::Services, Rect::new(12, 2, 10, 1))]);
    let event = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 13,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        translate_event(event, &regions, None),
        Some(Action::SelectTab(Tab::Services))
    );
}

#[test]
fn interval_button_clicks_step_the_interval() {
    let regions = UiRegions::default().with_interval_buttons([
        (IntervalStep::Shorter, Rect::new(100, 0, 3, 1)),
        (IntervalStep::Longer, Rect::new(104, 0, 3, 1)),
    ]);
    let click = |column| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    };

    assert_eq!(
        translate_event(click(101), &regions, None),
        Some(Action::StepSamplingInterval(IntervalStep::Shorter))
    );
    assert_eq!(
        translate_event(click(106), &regions, None),
        Some(Action::StepSamplingInterval(IntervalStep::Longer))
    );
    assert_eq!(translate_event(click(103), &regions, None), None);
}

#[test]
fn clicks_outside_tabs_are_ignored() {
    let regions = UiRegions::from_tabs([(Tab::Overview, Rect::new(1, 1, 10, 1))]);
    let event = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 20,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(translate_event(event, &regions, None), None);
}

#[test]
fn process_row_click_and_wheel_are_semantic_actions() {
    let identity = ProcessIdentity {
        pid: 42,
        start_time: 9001,
    };
    let regions =
        UiRegions::from_process_rows([(identity, Rect::new(2, 5, 40, 1))], InputMode::Normal);
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 8,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    let scroll = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 8,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        translate_event(click, &regions, None),
        Some(Action::SelectProcess(identity))
    );
    assert_eq!(
        translate_event(scroll, &regions, None),
        Some(Action::ProcessNext)
    );
}

#[test]
fn service_row_click_and_wheel_are_semantic_actions() {
    let regions = UiRegions::from_service_rows(
        [(std::sync::Arc::from("sshd.service"), Rect::new(2, 5, 40, 1))],
        InputMode::Services,
    );
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 8,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    let scroll = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 8,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        translate_event(click, &regions, None),
        Some(Action::SelectService("sshd.service".into()))
    );
    assert_eq!(
        translate_event(scroll, &regions, None),
        Some(Action::ServiceNext)
    );
}

#[test]
fn log_row_click_and_wheel_are_semantic_actions() {
    let regions = UiRegions::from_log_rows([(41, Rect::new(2, 5, 40, 1))], InputMode::Logs);
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 8,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    let scroll = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 8,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        translate_event(click, &regions, None),
        Some(Action::SelectLog(41))
    );
    assert_eq!(
        translate_event(scroll, &regions, None),
        Some(Action::LogPrevious)
    );
}

#[test]
fn pin_control_clicks_move_the_pin_and_their_hover_is_the_row() {
    let identity = ProcessIdentity {
        pid: 42,
        start_time: 9001,
    };
    let regions =
        UiRegions::from_process_rows([(identity, Rect::new(0, 5, 80, 1))], InputMode::Normal)
            .with_pin_controls(Rect::new(76, 5, 2, 1), Rect::new(78, 5, 2, 1));
    let mouse = |kind, column| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        })
    };
    let click = MouseEventKind::Down(MouseButton::Left);

    assert_eq!(
        translate_event(mouse(click, 77), &regions, None),
        Some(Action::MovePin(identity, PinMove::Up))
    );
    assert_eq!(
        translate_event(mouse(click, 78), &regions, None),
        Some(Action::MovePin(identity, PinMove::Down))
    );
    assert_eq!(
        translate_event(mouse(click, 75), &regions, None),
        Some(Action::SelectProcess(identity))
    );

    // Row → ▲ → ▼ → row: one hover transition, then nothing.
    let mut hovered = None;
    let mut dispatched = 0;
    for column in [10, 76, 77, 78, 79, 20] {
        if let Some(Action::HoverMouseTarget(target)) = translate_event(
            mouse(MouseEventKind::Moved, column),
            &regions,
            hovered.as_ref(),
        ) {
            hovered = target;
            dispatched += 1;
        }
    }
    assert_eq!(dispatched, 1);
    assert_eq!(hovered, Some(MouseTarget::ProcessRow(identity)));
}

#[test]
fn wheel_outside_process_body_is_ignored() {
    let regions = UiRegions::from_process_rows(
        [(
            ProcessIdentity {
                pid: 42,
                start_time: 9001,
            },
            Rect::new(2, 5, 40, 1),
        )],
        InputMode::Normal,
    );
    let scroll = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 8,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(translate_event(scroll, &regions, None), None);
}

#[test]
fn mouse_motion_updates_semantic_hover_target() {
    let regions =
        UiRegions::from_process_headers([(ProcessSortField::Cpu, Rect::new(30, 4, 10, 1))]);
    let over_header = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 32,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });
    let outside = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        translate_event(over_header, &regions, None),
        Some(Action::HoverMouseTarget(Some(
            MouseTarget::ProcessSortHeader(ProcessSortField::Cpu)
        )))
    );
    assert_eq!(
        translate_event(
            outside,
            &regions,
            Some(&MouseTarget::ProcessSortHeader(ProcessSortField::Cpu))
        ),
        Some(Action::HoverMouseTarget(None))
    );
}

#[test]
fn repeated_motion_within_one_target_dispatches_once() {
    let target = MouseTarget::ProcessSortHeader(ProcessSortField::Cpu);
    let regions =
        UiRegions::from_process_headers([(ProcessSortField::Cpu, Rect::new(30, 4, 10, 1))]);
    let mut hovered = None;
    let mut dispatched = 0;

    for _ in 0..100 {
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 32,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        if let Some(Action::HoverMouseTarget(next)) =
            translate_event(event, &regions, hovered.as_ref())
        {
            hovered = next;
            dispatched += 1;
        }
    }

    assert_eq!(hovered, Some(target));
    assert_eq!(dispatched, 1);
}

#[test]
fn motion_over_whitespace_is_ignored_when_hover_is_already_clear() {
    let event = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(translate_event(event, &no_regions(), None), None);
}

#[test]
fn search_mode_treats_q_as_query_text() {
    let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);

    assert_eq!(
        translate_key_event(key, InputMode::ProcessSearch),
        Some(Action::AppendProcessSearch('q'))
    );
}

const SEARCH_MODES: [(InputMode, [Action; 4]); 3] = [
    (
        InputMode::ProcessSearch,
        [
            Action::ProcessPrevious,
            Action::ProcessNext,
            Action::ProcessPreviousPage,
            Action::ProcessNextPage,
        ],
    ),
    (
        InputMode::ServiceSearch,
        [
            Action::ServicePrevious,
            Action::ServiceNext,
            Action::ServicePreviousPage,
            Action::ServiceNextPage,
        ],
    ),
    (
        InputMode::LogSearch,
        [
            Action::LogPrevious,
            Action::LogNext,
            Action::LogPreviousPage,
            Action::LogNextPage,
        ],
    ),
];

#[test]
fn search_modes_map_arrow_and_page_keys_to_navigation() {
    for (mode, actions) in SEARCH_MODES {
        let keys = [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ];
        for (code, action) in keys.into_iter().zip(actions) {
            for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                assert_eq!(
                    translate_key_event(KeyEvent::new(code, modifiers), mode),
                    Some(action.clone()),
                    "{code:?} {modifiers:?} in {mode:?}"
                );
            }
        }
    }
}

#[test]
fn search_modes_keep_letters_as_query_text() {
    for (mode, _) in SEARCH_MODES {
        for character in ['j', 'k', 'J', 'K', 'q', '/', ' '] {
            let action = translate_key_event(
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                mode,
            );
            let expected = match mode {
                InputMode::ProcessSearch => Action::AppendProcessSearch(character),
                InputMode::ServiceSearch => Action::AppendServiceSearch(character),
                _ => Action::AppendLogSearch(character),
            };
            assert_eq!(action, Some(expected), "{character:?} in {mode:?}");
        }
    }
}

#[test]
fn search_modes_leave_home_and_end_unmapped() {
    for (mode, _) in SEARCH_MODES {
        for code in [KeyCode::Home, KeyCode::End] {
            assert_eq!(
                translate_key_event(KeyEvent::new(code, KeyModifiers::NONE), mode),
                None,
                "{code:?} in {mode:?}"
            );
        }
    }
}

#[test]
fn plus_and_minus_step_the_interval_on_tabs_and_are_text_in_search() {
    let plus = KeyEvent::new(KeyCode::Char('+'), KeyModifiers::SHIFT);
    let minus = KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE);
    for mode in TAB_MODES {
        assert_eq!(
            translate_key_event(plus, mode),
            Some(Action::StepSamplingInterval(IntervalStep::Longer))
        );
        assert_eq!(
            translate_key_event(minus, mode),
            Some(Action::StepSamplingInterval(IntervalStep::Shorter))
        );
    }
    assert_eq!(
        translate_key_event(minus, InputMode::ProcessSearch),
        Some(Action::AppendProcessSearch('-'))
    );
    for mode in [
        InputMode::Help,
        InputMode::ProcessDetail,
        InputMode::ProcessSignalConfirm,
        InputMode::LogDetail,
    ] {
        assert_eq!(translate_key_event(plus, mode), None, "{mode:?}");
    }
}

#[test]
fn service_keys_map_to_service_actions() {
    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            InputMode::Services,
        ),
        Some(Action::ServiceNext)
    );
    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            InputMode::Services,
        ),
        Some(Action::BeginServiceSearch)
    );
    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            InputMode::Services,
        ),
        Some(Action::RefreshServices)
    );
    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE),
            InputMode::ServiceSearch,
        ),
        Some(Action::AppendServiceSearch('Q'))
    );
}

#[test]
fn log_keys_map_to_log_actions() {
    for (key, action) in [
        (KeyCode::Char('j'), Action::LogNext),
        (KeyCode::Char('/'), Action::BeginLogSearch),
        (KeyCode::Char('f'), Action::ToggleLogFollow),
        (KeyCode::Char(' '), Action::ToggleLogPause),
        (KeyCode::Enter, Action::OpenLogDetails),
    ] {
        assert_eq!(
            translate_key_event(KeyEvent::new(key, KeyModifiers::NONE), InputMode::Logs),
            Some(action)
        );
    }

    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE),
            InputMode::LogSearch,
        ),
        Some(Action::AppendLogSearch('F'))
    );
}

#[test]
fn process_sort_keys_become_sort_actions() {
    for (key, field) in [
        ('c', ProcessSortField::Cpu),
        ('m', ProcessSortField::Memory),
        ('p', ProcessSortField::Pid),
        ('n', ProcessSortField::Name),
    ] {
        assert_eq!(
            translate_key_event(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                InputMode::Normal,
            ),
            Some(Action::SortProcesses(field))
        );
    }
}

#[test]
fn pin_keys_do_not_shadow_sorting_or_navigation() {
    let key =
        |code, modifiers| translate_key_event(KeyEvent::new(code, modifiers), InputMode::Normal);

    assert_eq!(
        key(KeyCode::Char('P'), KeyModifiers::NONE),
        Some(Action::TogglePin)
    );
    assert_eq!(
        key(KeyCode::Char('P'), KeyModifiers::SHIFT),
        Some(Action::TogglePin)
    );
    // Kitty keyboard protocol reports Shift+p as a lowercase key with SHIFT.
    assert_eq!(
        key(KeyCode::Char('p'), KeyModifiers::SHIFT),
        Some(Action::TogglePin)
    );
    assert_eq!(
        key(KeyCode::Char('p'), KeyModifiers::NONE),
        Some(Action::SortProcesses(ProcessSortField::Pid))
    );

    for modifiers in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
        assert_eq!(
            key(KeyCode::Up, modifiers),
            Some(Action::MoveSelectedPin(PinMove::Up))
        );
        assert_eq!(
            key(KeyCode::Down, modifiers),
            Some(Action::MoveSelectedPin(PinMove::Down))
        );
    }
    assert_eq!(
        key(KeyCode::Up, KeyModifiers::NONE),
        Some(Action::ProcessPrevious)
    );
    assert_eq!(
        key(KeyCode::Down, KeyModifiers::NONE),
        Some(Action::ProcessNext)
    );

    // While typing a search, P is query text and Shift+arrows navigate.
    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
            InputMode::ProcessSearch
        ),
        Some(Action::AppendProcessSearch('P'))
    );
    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
            InputMode::ProcessSearch
        ),
        Some(Action::ProcessPrevious)
    );
}

#[test]
fn v_cycles_the_view_on_filterable_screens_and_is_text_in_search() {
    let v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE);
    for mode in [InputMode::Normal, InputMode::Services, InputMode::Logs] {
        assert_eq!(translate_key_event(v, mode), Some(Action::CycleViewFilter));
    }
    assert_eq!(translate_key_event(v, InputMode::Network), None);
    assert_eq!(
        translate_key_event(v, InputMode::LogSearch),
        Some(Action::AppendLogSearch('v'))
    );
}

#[test]
fn process_header_click_becomes_sort_action() {
    let regions =
        UiRegions::from_process_headers([(ProcessSortField::Memory, Rect::new(30, 4, 12, 1))]);
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 31,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        translate_event(click, &regions, None),
        Some(Action::SortProcesses(ProcessSortField::Memory))
    );
}

#[test]
fn resize_becomes_resize_action() {
    assert_eq!(
        translate_event(Event::Resize(80, 24), &no_regions(), None),
        Some(Action::Resize)
    );
}

#[test]
fn network_keys_map_to_network_actions() {
    for (key, action) in [
        (KeyCode::Char('k'), Action::NetworkPrevious),
        (KeyCode::Char('j'), Action::NetworkNext),
        (KeyCode::Up, Action::NetworkPrevious),
        (KeyCode::Down, Action::NetworkNext),
        (KeyCode::Home, Action::NetworkFirst),
        (KeyCode::End, Action::NetworkLast),
        (KeyCode::PageUp, Action::NetworkPreviousPage),
        (KeyCode::PageDown, Action::NetworkNextPage),
        (KeyCode::Enter, Action::OpenNetworkDetails),
    ] {
        assert_eq!(
            translate_key_event(KeyEvent::new(key, KeyModifiers::NONE), InputMode::Network),
            Some(action)
        );
    }

    assert_eq!(
        translate_key_event(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            InputMode::NetworkDetail
        ),
        Some(Action::Escape)
    );
}

#[test]
fn network_row_click_and_wheel_are_semantic_actions() {
    let regions = UiRegions::from_network_rows(
        [(Arc::from("enp6s0"), Rect::new(0, 5, 80, 1))],
        InputMode::Network,
    );
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        translate_event(click, &regions, None),
        Some(Action::SelectNetwork(Arc::from("enp6s0")))
    );

    let wheel = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        translate_event(wheel, &regions, None),
        Some(Action::NetworkNext)
    );
}

#[test]
fn process_signal_keys_and_modal_events() {
    let empty_regions = UiRegions::default();

    // Normal mode shortcuts
    for t_key in [
        KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::SHIFT),
    ] {
        assert_eq!(
            translate_event(Event::Key(t_key), &empty_regions, None),
            Some(Action::RequestProcessSignal(
                crate::linux::ProcessSignal::Term
            )),
            "{t_key:?}"
        );
    }
    // Signals need Shift; a plain `t` does nothing.
    let t_plain = Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
    assert_eq!(translate_event(t_plain, &empty_regions, None), None);

    let k_upper = Event::Key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE));
    assert_eq!(
        translate_event(k_upper, &empty_regions, None),
        Some(Action::RequestProcessSignal(
            crate::linux::ProcessSignal::Kill
        ))
    );

    let k_shift = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT));
    assert_eq!(
        translate_event(k_shift, &empty_regions, None),
        Some(Action::RequestProcessSignal(
            crate::linux::ProcessSignal::Kill
        ))
    );

    // Modal keys
    let modal_regions =
        UiRegions::from_process_signal_buttons(Rect::new(10, 5, 12, 1), Rect::new(26, 5, 15, 1));

    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        translate_event(esc, &modal_regions, None),
        Some(Action::CancelProcessSignal)
    );

    let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        translate_event(tab, &modal_regions, None),
        Some(Action::ToggleProcessSignalFocus)
    );

    let left = Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(
        translate_event(left, &modal_regions, None),
        Some(Action::FocusProcessSignal(
            crate::action::SignalConfirmButton::Cancel
        ))
    );

    let right = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        translate_event(right, &modal_regions, None),
        Some(Action::FocusProcessSignal(
            crate::action::SignalConfirmButton::Confirm
        ))
    );

    let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        translate_event(enter, &modal_regions, None),
        Some(Action::ExecuteFocusedProcessSignal)
    );

    // Mouse click on Cancel button
    let click_cancel = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 12,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        translate_event(click_cancel, &modal_regions, None),
        Some(Action::CancelProcessSignal)
    );

    // Mouse click on Confirm button
    let click_confirm = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 28,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        translate_event(click_confirm, &modal_regions, None),
        Some(Action::ConfirmProcessSignal)
    );

    // Mouse click outside buttons returns None
    let click_outside = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 50,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(translate_event(click_outside, &modal_regions, None), None);
}

#[test]
fn ctrl_c_always_triggers_quit_in_all_input_modes() {
    let modes = [
        InputMode::Normal,
        InputMode::Help,
        InputMode::ProcessSearch,
        InputMode::ProcessDetail,
        InputMode::ProcessSignalConfirm,
        InputMode::ServiceSearch,
        InputMode::ServiceDetail,
        InputMode::LogSearch,
        InputMode::LogDetail,
        InputMode::NetworkDetail,
        InputMode::Menu,
        InputMode::About,
    ];

    let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    let ctrl_upper_c = Event::Key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL));

    for mode in modes {
        let regions = UiRegions::default().with_input_mode(mode);

        assert_eq!(
            translate_event(ctrl_c.clone(), &regions, None),
            Some(Action::Quit),
            "Ctrl+c failed in mode {mode:?}"
        );
        assert_eq!(
            translate_event(ctrl_upper_c.clone(), &regions, None),
            Some(Action::Quit),
            "Ctrl+C failed in mode {mode:?}"
        );
    }
}

#[test]
fn q_asks_to_quit_outside_the_menu_and_confirms_inside_it() {
    let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    for mode in [
        InputMode::Normal,
        InputMode::Services,
        InputMode::Logs,
        InputMode::Network,
        InputMode::ProcessSignalConfirm,
        InputMode::ProcessDetail,
        InputMode::ServiceDetail,
        InputMode::LogDetail,
        InputMode::NetworkDetail,
        InputMode::Help,
        InputMode::About,
    ] {
        assert_eq!(
            translate_key_event(q, mode),
            Some(Action::RequestQuit),
            "{mode:?}"
        );
    }
    assert_eq!(translate_key_event(q, InputMode::Menu), Some(Action::Quit));
}

#[test]
fn menu_keys_navigate_select_and_quit() {
    let key = |code| translate_key_event(KeyEvent::new(code, KeyModifiers::NONE), InputMode::Menu);
    assert_eq!(key(KeyCode::Up), Some(Action::MenuPrevious));
    assert_eq!(key(KeyCode::Char('k')), Some(Action::MenuPrevious));
    assert_eq!(key(KeyCode::Down), Some(Action::MenuNext));
    assert_eq!(key(KeyCode::Char('j')), Some(Action::MenuNext));
    assert_eq!(key(KeyCode::Enter), Some(Action::ActivateSelectedMenuItem));
    assert_eq!(key(KeyCode::Esc), Some(Action::Escape));
    assert_eq!(key(KeyCode::Char('q')), Some(Action::Quit), "q confirms");
    assert_eq!(key(KeyCode::Char('2')), None, "tabs are blocked");

    let about =
        |code| translate_key_event(KeyEvent::new(code, KeyModifiers::NONE), InputMode::About);
    assert_eq!(about(KeyCode::Esc), Some(Action::Escape));
    assert_eq!(about(KeyCode::Enter), None);
}

#[test]
fn question_mark_toggles_help_closed() {
    let help_regions = UiRegions::default().with_input_mode(InputMode::Help);
    let question_key = Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    assert_eq!(
        translate_event(question_key.clone(), &help_regions, None),
        Some(Action::Escape)
    );

    let detail_regions = UiRegions::default().with_input_mode(InputMode::ProcessDetail);
    assert_eq!(translate_event(question_key, &detail_regions, None), None);
}
