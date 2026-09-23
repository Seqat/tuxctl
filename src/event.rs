use std::{
    io,
    time::{Duration, Instant},
};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use crate::{
    action::{Action, InputMode, IntervalStep, MouseTarget, PinMove, ProcessSortField, Tab},
    ui::UiRegions,
};

pub struct EventHandler {
    tick_rate: Duration,
    next_tick: Instant,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        Self {
            tick_rate,
            next_tick: Instant::now() + tick_rate,
        }
    }

    pub fn next_action(
        &mut self,
        regions: &UiRegions,
        hovered: Option<&MouseTarget>,
        redraw_deadline: Option<Instant>,
    ) -> io::Result<Option<Action>> {
        loop {
            let now = Instant::now();
            if redraw_deadline.is_some_and(|deadline| now >= deadline) {
                return Ok(None);
            }
            if now >= self.next_tick {
                self.next_tick = now + self.tick_rate;
                return Ok(Some(Action::Tick(now)));
            }

            let deadline = redraw_deadline
                .map(|deadline| deadline.min(self.next_tick))
                .unwrap_or(self.next_tick);
            let timeout = deadline.saturating_duration_since(now);

            if !event::poll(timeout)? {
                continue;
            }

            if let Some(action) = translate_event(event::read()?, regions, hovered) {
                return Ok(Some(action));
            }
        }
    }

    pub fn poll_action(
        &mut self,
        regions: &UiRegions,
        hovered: Option<&MouseTarget>,
    ) -> io::Result<Option<Action>> {
        while event::poll(Duration::ZERO)? {
            if let Some(action) = translate_event(event::read()?, regions, hovered) {
                return Ok(Some(action));
            }
        }
        Ok(None)
    }
}

fn translate_event(
    event: Event,
    regions: &UiRegions,
    hovered: Option<&MouseTarget>,
) -> Option<Action> {
    match event {
        Event::Key(key) => translate_key_event(key, regions.input_mode()),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => regions
                .target_at(mouse.column, mouse.row)
                .map(action_for_mouse_target),
            MouseEventKind::Moved => {
                let target = regions.target_at(mouse.column, mouse.row);
                (target.as_ref() != hovered).then_some(Action::HoverMouseTarget(target))
            }
            MouseEventKind::ScrollUp if regions.process_scroll_at(mouse.column, mouse.row) => {
                Some(Action::ProcessPrevious)
            }
            MouseEventKind::ScrollDown if regions.process_scroll_at(mouse.column, mouse.row) => {
                Some(Action::ProcessNext)
            }
            MouseEventKind::ScrollUp if regions.service_scroll_at(mouse.column, mouse.row) => {
                Some(Action::ServicePrevious)
            }
            MouseEventKind::ScrollDown if regions.service_scroll_at(mouse.column, mouse.row) => {
                Some(Action::ServiceNext)
            }
            MouseEventKind::ScrollUp if regions.log_scroll_at(mouse.column, mouse.row) => {
                Some(Action::LogPrevious)
            }
            MouseEventKind::ScrollDown if regions.log_scroll_at(mouse.column, mouse.row) => {
                Some(Action::LogNext)
            }
            MouseEventKind::ScrollUp if regions.network_scroll_at(mouse.column, mouse.row) => {
                Some(Action::NetworkPrevious)
            }
            MouseEventKind::ScrollDown if regions.network_scroll_at(mouse.column, mouse.row) => {
                Some(Action::NetworkNext)
            }
            _ => None,
        },
        Event::Resize(_, _) => Some(Action::Resize),
        _ => None,
    }
}

fn action_for_mouse_target(target: MouseTarget) -> Action {
    match target {
        MouseTarget::Tab(tab) => Action::SelectTab(tab),
        MouseTarget::ProcessRow(identity) => Action::SelectProcess(identity),
        MouseTarget::ProcessSortHeader(field) => Action::SortProcesses(field),
        MouseTarget::ProcessSignalCancel => Action::CancelProcessSignal,
        MouseTarget::ProcessSignalConfirm => Action::ConfirmProcessSignal,
        MouseTarget::ServiceRow(unit) => Action::SelectService(unit),
        MouseTarget::LogRow(id) => Action::SelectLog(id),
        MouseTarget::NetworkRow(name) => Action::SelectNetwork(name),
    }
}

fn translate_key_event(key: KeyEvent, input_mode: InputMode) -> Option<Action> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Some(Action::Quit);
    }

    if input_mode == InputMode::ProcessSearch {
        return match key.code {
            KeyCode::Esc => Some(Action::Escape),
            KeyCode::Enter => Some(Action::OpenProcessDetails),
            KeyCode::Backspace => Some(Action::BackspaceProcessSearch),
            // Arrows move through the matches; letters (including j/k) stay query text.
            KeyCode::Up => Some(Action::ProcessPrevious),
            KeyCode::Down => Some(Action::ProcessNext),
            KeyCode::PageUp => Some(Action::ProcessPreviousPage),
            KeyCode::PageDown => Some(Action::ProcessNextPage),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(Action::AppendProcessSearch(character))
            }
            _ => None,
        };
    }

    if input_mode == InputMode::ServiceSearch {
        return match key.code {
            KeyCode::Esc => Some(Action::Escape),
            KeyCode::Enter => Some(Action::OpenServiceDetails),
            KeyCode::Backspace => Some(Action::BackspaceServiceSearch),
            KeyCode::Up => Some(Action::ServicePrevious),
            KeyCode::Down => Some(Action::ServiceNext),
            KeyCode::PageUp => Some(Action::ServicePreviousPage),
            KeyCode::PageDown => Some(Action::ServiceNextPage),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(Action::AppendServiceSearch(character))
            }
            _ => None,
        };
    }

    if input_mode == InputMode::LogSearch {
        return match key.code {
            KeyCode::Esc => Some(Action::Escape),
            KeyCode::Enter => Some(Action::OpenLogDetails),
            KeyCode::Backspace => Some(Action::BackspaceLogSearch),
            KeyCode::Up => Some(Action::LogPrevious),
            KeyCode::Down => Some(Action::LogNext),
            KeyCode::PageUp => Some(Action::LogPreviousPage),
            KeyCode::PageDown => Some(Action::LogNextPage),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(Action::AppendLogSearch(character))
            }
            _ => None,
        };
    }

    if input_mode == InputMode::ProcessSignalConfirm {
        return match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Esc => Some(Action::CancelProcessSignal),
            KeyCode::Left | KeyCode::Char('h') => Some(Action::FocusProcessSignal(
                crate::action::SignalConfirmButton::Cancel,
            )),
            KeyCode::Right | KeyCode::Char('l') => Some(Action::FocusProcessSignal(
                crate::action::SignalConfirmButton::Confirm,
            )),
            KeyCode::Tab | KeyCode::BackTab => Some(Action::ToggleProcessSignalFocus),
            KeyCode::Enter => Some(Action::ExecuteFocusedProcessSignal),
            _ => None,
        };
    }

    if matches!(
        input_mode,
        InputMode::Help
            | InputMode::ProcessDetail
            | InputMode::ServiceDetail
            | InputMode::LogDetail
            | InputMode::NetworkDetail
    ) {
        return match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Esc => Some(Action::Escape),
            KeyCode::Char('?') if input_mode == InputMode::Help => Some(Action::Escape),
            _ => None,
        };
    }

    // Tab screens share the global keys; screen keys never shadow them (tested).
    global_tab_key(key).or_else(|| match input_mode {
        InputMode::Services => service_key(key),
        InputMode::Logs => log_key(key),
        InputMode::Network => network_key(key),
        _ => process_key(key),
    })
}

/// Keys that behave the same on every tab screen.
fn global_tab_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('1') => Some(Action::SelectTab(Tab::Overview)),
        KeyCode::Char('2') => Some(Action::SelectTab(Tab::Processes)),
        KeyCode::Char('3') => Some(Action::SelectTab(Tab::Services)),
        KeyCode::Char('4') => Some(Action::SelectTab(Tab::Logs)),
        KeyCode::Char('5') => Some(Action::SelectTab(Tab::Network)),
        KeyCode::Char('?') => Some(Action::ShowHelp),
        KeyCode::Char('+') => Some(Action::StepSamplingInterval(IntervalStep::Longer)),
        KeyCode::Char('-') => Some(Action::StepSamplingInterval(IntervalStep::Shorter)),
        KeyCode::Esc => Some(Action::Escape),
        KeyCode::BackTab => Some(Action::PreviousTab),
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => Some(Action::PreviousTab),
        KeyCode::Tab | KeyCode::Right => Some(Action::NextTab),
        KeyCode::Left => Some(Action::PreviousTab),
        _ => None,
    }
}

/// Overview and Processes (InputMode::Normal).
fn process_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('t') => Some(Action::RequestProcessSignal(
            crate::linux::ProcessSignal::Term,
        )),
        KeyCode::Char('K') => Some(Action::RequestProcessSignal(
            crate::linux::ProcessSignal::Kill,
        )),
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::SHIFT) => Some(
            Action::RequestProcessSignal(crate::linux::ProcessSignal::Kill),
        ),
        // Shifted forms come before `p` (sort) and plain arrows (navigation).
        KeyCode::Char('P') => Some(Action::TogglePin),
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(Action::TogglePin)
        }
        KeyCode::Up
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            Some(Action::MoveSelectedPin(PinMove::Up))
        }
        KeyCode::Down
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            Some(Action::MoveSelectedPin(PinMove::Down))
        }
        KeyCode::Up | KeyCode::Char('k') => Some(Action::ProcessPrevious),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::ProcessNext),
        KeyCode::PageUp => Some(Action::ProcessPreviousPage),
        KeyCode::PageDown => Some(Action::ProcessNextPage),
        KeyCode::Home => Some(Action::ProcessFirst),
        KeyCode::End => Some(Action::ProcessLast),
        KeyCode::Char('/') => Some(Action::BeginProcessSearch),
        KeyCode::Enter => Some(Action::OpenProcessDetails),
        KeyCode::Char('c') => Some(Action::SortProcesses(ProcessSortField::Cpu)),
        KeyCode::Char('m') => Some(Action::SortProcesses(ProcessSortField::Memory)),
        KeyCode::Char('p') => Some(Action::SortProcesses(ProcessSortField::Pid)),
        KeyCode::Char('n') => Some(Action::SortProcesses(ProcessSortField::Name)),
        _ => None,
    }
}

fn service_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(Action::ServicePrevious),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::ServiceNext),
        KeyCode::PageUp => Some(Action::ServicePreviousPage),
        KeyCode::PageDown => Some(Action::ServiceNextPage),
        KeyCode::Home => Some(Action::ServiceFirst),
        KeyCode::End => Some(Action::ServiceLast),
        KeyCode::Char('/') => Some(Action::BeginServiceSearch),
        KeyCode::Enter => Some(Action::OpenServiceDetails),
        KeyCode::Char('r') => Some(Action::RefreshServices),
        _ => None,
    }
}

fn log_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(Action::LogPrevious),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::LogNext),
        KeyCode::PageUp => Some(Action::LogPreviousPage),
        KeyCode::PageDown => Some(Action::LogNextPage),
        KeyCode::Home => Some(Action::LogFirst),
        KeyCode::End => Some(Action::LogLast),
        KeyCode::Char('/') => Some(Action::BeginLogSearch),
        KeyCode::Enter => Some(Action::OpenLogDetails),
        KeyCode::Char('f') => Some(Action::ToggleLogFollow),
        KeyCode::Char(' ') => Some(Action::ToggleLogPause),
        _ => None,
    }
}

fn network_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(Action::NetworkPrevious),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::NetworkNext),
        KeyCode::PageUp => Some(Action::NetworkPreviousPage),
        KeyCode::PageDown => Some(Action::NetworkNextPage),
        KeyCode::Home => Some(Action::NetworkFirst),
        KeyCode::End => Some(Action::NetworkLast),
        KeyCode::Enter => Some(Action::OpenNetworkDetails),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::MouseEvent;
    use ratatui::layout::Rect;

    use super::*;
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
            ("processes", process_key),
            ("services", service_key),
            ("logs", log_key),
            ("network", network_key),
        ];
        for key in candidate_keys() {
            if global_tab_key(key).is_none() {
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
            let Some(global) = global_tab_key(key) else {
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

    /// Maps a key as written in the README tables to a key event.
    fn readme_key(token: &str) -> KeyEvent {
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
            "Shift+P" => KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT),
            "Shift+↑" => KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
            "Shift+↓" => KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
            "Alt+↑" => KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
            "Alt+↓" => KeyEvent::new(KeyCode::Down, KeyModifiers::ALT),
            single if single.chars().count() == 1 => {
                plain(KeyCode::Char(single.chars().next().unwrap()))
            }
            other => panic!("README key {other:?} has no mapping in readme_key()"),
        }
    }

    #[test]
    fn every_key_in_the_readme_tables_is_handled() {
        let readme = include_str!("../README.md");
        let controls = readme
            .split("## Controls & Keybindings")
            .nth(1)
            .and_then(|rest| rest.split("### Mouse Controls").next())
            .expect("README controls section");
        let mut modes: &[InputMode] = &[];
        let mut checked = 0;
        for line in controls.lines() {
            if let Some(heading) = line.trim_start_matches('#').strip_prefix(' ') {
                if line.starts_with('#') {
                    modes = match heading {
                        "Global Controls" | "Navigation & Common Actions" => &TAB_MODES,
                        "Processes" => &[InputMode::Normal],
                        "Signal Confirmation" => &[InputMode::ProcessSignalConfirm],
                        "Services" => &[InputMode::Services],
                        "Logs" => &[InputMode::Logs],
                        other => panic!("unknown README controls section {other:?}"),
                    };
                    continue;
                }
            }
            let Some(keys) = line.strip_prefix("| `") else {
                continue;
            };
            let first_column = keys.split(" | ").next().unwrap_or_default();
            for token in format!("`{first_column}").split('`').skip(1).step_by(2) {
                let key = readme_key(token);
                for &mode in modes {
                    // The Network screen has no search; the README lists `/` as common.
                    if token == "/" && mode == InputMode::Network {
                        continue;
                    }
                    assert!(
                        translate_key_event(key, mode).is_some(),
                        "README documents `{token}` but {mode:?} ignores it"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked >= 30, "only {checked} README keys were checked");
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
        let key = |code, modifiers| {
            translate_key_event(KeyEvent::new(code, modifiers), InputMode::Normal)
        };

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
        let t_key = Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(
            translate_event(t_key, &empty_regions, None),
            Some(Action::RequestProcessSignal(
                crate::linux::ProcessSignal::Term
            ))
        );

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
        let modal_regions = UiRegions::from_process_signal_buttons(
            Rect::new(10, 5, 12, 1),
            Rect::new(26, 5, 15, 1),
        );

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
}
