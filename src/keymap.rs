//! Key bindings: one table per input mode, the single place where a key is
//! assigned to an `Action`.
//!
//! Tables are searched in order and the first match wins, so a binding that
//! needs a modifier (`Shift+Tab`, `Shift+↑`) must come before the plain key.
//!
//! Convention: keys that change the system or persistent state (signals,
//! pins) need `Shift` or an uppercase letter; navigation, sorting, views,
//! search and menus use plain keys (tested below).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    action::{
        Action, InputMode, IntervalStep, PinMove, ProcessSortField, SignalConfirmButton, Tab,
    },
    linux::ProcessSignal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mods {
    /// Matches whatever modifiers are held.
    Any,
    /// Matches when at least one of these modifiers is held.
    AnyOf(KeyModifiers),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPattern {
    code: KeyCode,
    mods: Mods,
}

impl KeyPattern {
    fn matches(self, key: KeyEvent) -> bool {
        self.code == key.code
            && match self.mods {
                Mods::Any => true,
                Mods::AnyOf(mods) => key.modifiers.intersects(mods),
            }
    }
}

#[derive(Debug)]
pub struct Binding {
    pub key: KeyPattern,
    pub action: Action,
}

const fn bind(code: KeyCode, action: Action) -> Binding {
    Binding {
        key: KeyPattern {
            code,
            mods: Mods::Any,
        },
        action,
    }
}

const fn bind_with(code: KeyCode, mods: KeyModifiers, action: Action) -> Binding {
    Binding {
        key: KeyPattern {
            code,
            mods: Mods::AnyOf(mods),
        },
        action,
    }
}

const fn ch(character: char) -> KeyCode {
    KeyCode::Char(character)
}

const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
const SHIFT_OR_ALT: KeyModifiers = KeyModifiers::SHIFT.union(KeyModifiers::ALT);

/// Keys that behave the same on every tab screen; screen tables never shadow them.
pub const GLOBAL: &[Binding] = &[
    bind(ch('q'), Action::RequestQuit),
    bind(ch('1'), Action::SelectTab(Tab::Overview)),
    bind(ch('2'), Action::SelectTab(Tab::Processes)),
    bind(ch('3'), Action::SelectTab(Tab::Services)),
    bind(ch('4'), Action::SelectTab(Tab::Logs)),
    bind(ch('5'), Action::SelectTab(Tab::Network)),
    bind(ch('?'), Action::ShowHelp),
    bind(ch('+'), Action::StepSamplingInterval(IntervalStep::Longer)),
    bind(ch('-'), Action::StepSamplingInterval(IntervalStep::Shorter)),
    bind(KeyCode::Esc, Action::Escape),
    bind(KeyCode::BackTab, Action::PreviousTab),
    bind_with(KeyCode::Tab, SHIFT, Action::PreviousTab),
    bind(KeyCode::Tab, Action::NextTab),
    bind(KeyCode::Right, Action::NextTab),
    bind(KeyCode::Left, Action::PreviousTab),
];

/// Overview and Processes (`InputMode::Normal`).
pub const PROCESSES: &[Binding] = &[
    bind(ch('T'), Action::RequestProcessSignal(ProcessSignal::Term)),
    bind_with(
        ch('t'),
        SHIFT,
        Action::RequestProcessSignal(ProcessSignal::Term),
    ),
    bind(ch('K'), Action::RequestProcessSignal(ProcessSignal::Kill)),
    bind_with(
        ch('k'),
        SHIFT,
        Action::RequestProcessSignal(ProcessSignal::Kill),
    ),
    bind(ch('P'), Action::TogglePin),
    bind_with(ch('p'), SHIFT, Action::TogglePin),
    bind_with(
        KeyCode::Up,
        SHIFT_OR_ALT,
        Action::MoveSelectedPin(PinMove::Up),
    ),
    bind_with(
        KeyCode::Down,
        SHIFT_OR_ALT,
        Action::MoveSelectedPin(PinMove::Down),
    ),
    bind(KeyCode::Up, Action::ProcessPrevious),
    bind(ch('k'), Action::ProcessPrevious),
    bind(KeyCode::Down, Action::ProcessNext),
    bind(ch('j'), Action::ProcessNext),
    bind(KeyCode::PageUp, Action::ProcessPreviousPage),
    bind(KeyCode::PageDown, Action::ProcessNextPage),
    bind(KeyCode::Home, Action::ProcessFirst),
    bind(KeyCode::End, Action::ProcessLast),
    bind(ch('/'), Action::BeginProcessSearch),
    bind(KeyCode::Enter, Action::OpenProcessDetails),
    bind(ch('c'), Action::SortProcesses(ProcessSortField::Cpu)),
    bind(ch('m'), Action::SortProcesses(ProcessSortField::Memory)),
    bind(ch('p'), Action::SortProcesses(ProcessSortField::Pid)),
    bind(ch('n'), Action::SortProcesses(ProcessSortField::Name)),
    bind(ch('v'), Action::CycleViewFilter),
];

pub const SERVICES: &[Binding] = &[
    bind(KeyCode::Up, Action::ServicePrevious),
    bind(ch('k'), Action::ServicePrevious),
    bind(KeyCode::Down, Action::ServiceNext),
    bind(ch('j'), Action::ServiceNext),
    bind(KeyCode::PageUp, Action::ServicePreviousPage),
    bind(KeyCode::PageDown, Action::ServiceNextPage),
    bind(KeyCode::Home, Action::ServiceFirst),
    bind(KeyCode::End, Action::ServiceLast),
    bind(ch('/'), Action::BeginServiceSearch),
    bind(KeyCode::Enter, Action::OpenServiceDetails),
    bind(ch('r'), Action::RefreshServices),
    bind(ch('v'), Action::CycleViewFilter),
];

pub const LOGS: &[Binding] = &[
    bind(KeyCode::Up, Action::LogPrevious),
    bind(ch('k'), Action::LogPrevious),
    bind(KeyCode::Down, Action::LogNext),
    bind(ch('j'), Action::LogNext),
    bind(KeyCode::PageUp, Action::LogPreviousPage),
    bind(KeyCode::PageDown, Action::LogNextPage),
    bind(KeyCode::Home, Action::LogFirst),
    bind(KeyCode::End, Action::LogLast),
    bind(ch('/'), Action::BeginLogSearch),
    bind(KeyCode::Enter, Action::OpenLogDetails),
    bind(ch('f'), Action::ToggleLogFollow),
    bind(ch(' '), Action::ToggleLogPause),
    bind(ch('v'), Action::CycleViewFilter),
];

pub const NETWORK: &[Binding] = &[
    bind(KeyCode::Up, Action::NetworkPrevious),
    bind(ch('k'), Action::NetworkPrevious),
    bind(KeyCode::Down, Action::NetworkNext),
    bind(ch('j'), Action::NetworkNext),
    bind(KeyCode::PageUp, Action::NetworkPreviousPage),
    bind(KeyCode::PageDown, Action::NetworkNextPage),
    bind(KeyCode::Home, Action::NetworkFirst),
    bind(KeyCode::End, Action::NetworkLast),
    bind(KeyCode::Enter, Action::OpenNetworkDetails),
];

// While searching, letters (including j/k) are query text; arrows move
// through the matches. Unbound characters are appended by `event.rs`.
pub const PROCESS_SEARCH: &[Binding] = &[
    bind(KeyCode::Esc, Action::Escape),
    bind(KeyCode::Enter, Action::OpenProcessDetails),
    bind(KeyCode::Backspace, Action::BackspaceProcessSearch),
    bind(KeyCode::Up, Action::ProcessPrevious),
    bind(KeyCode::Down, Action::ProcessNext),
    bind(KeyCode::PageUp, Action::ProcessPreviousPage),
    bind(KeyCode::PageDown, Action::ProcessNextPage),
];

pub const SERVICE_SEARCH: &[Binding] = &[
    bind(KeyCode::Esc, Action::Escape),
    bind(KeyCode::Enter, Action::OpenServiceDetails),
    bind(KeyCode::Backspace, Action::BackspaceServiceSearch),
    bind(KeyCode::Up, Action::ServicePrevious),
    bind(KeyCode::Down, Action::ServiceNext),
    bind(KeyCode::PageUp, Action::ServicePreviousPage),
    bind(KeyCode::PageDown, Action::ServiceNextPage),
];

pub const LOG_SEARCH: &[Binding] = &[
    bind(KeyCode::Esc, Action::Escape),
    bind(KeyCode::Enter, Action::OpenLogDetails),
    bind(KeyCode::Backspace, Action::BackspaceLogSearch),
    bind(KeyCode::Up, Action::LogPrevious),
    bind(KeyCode::Down, Action::LogNext),
    bind(KeyCode::PageUp, Action::LogPreviousPage),
    bind(KeyCode::PageDown, Action::LogNextPage),
];

pub const SIGNAL_CONFIRM: &[Binding] = &[
    bind(ch('q'), Action::RequestQuit),
    bind(KeyCode::Esc, Action::CancelProcessSignal),
    bind(
        KeyCode::Left,
        Action::FocusProcessSignal(SignalConfirmButton::Cancel),
    ),
    bind(
        ch('h'),
        Action::FocusProcessSignal(SignalConfirmButton::Cancel),
    ),
    bind(
        KeyCode::Right,
        Action::FocusProcessSignal(SignalConfirmButton::Confirm),
    ),
    bind(
        ch('l'),
        Action::FocusProcessSignal(SignalConfirmButton::Confirm),
    ),
    bind(KeyCode::Tab, Action::ToggleProcessSignalFocus),
    bind(KeyCode::BackTab, Action::ToggleProcessSignalFocus),
    bind(KeyCode::Enter, Action::ExecuteFocusedProcessSignal),
];

pub const MENU: &[Binding] = &[
    // `q` again confirms the quit that `q` asked for.
    bind(ch('q'), Action::Quit),
    bind(KeyCode::Esc, Action::Escape),
    bind(KeyCode::Up, Action::MenuPrevious),
    bind(ch('k'), Action::MenuPrevious),
    bind(KeyCode::Down, Action::MenuNext),
    bind(ch('j'), Action::MenuNext),
    bind(KeyCode::Enter, Action::ActivateSelectedMenuItem),
];

/// About and the detail popups.
pub const POPUP: &[Binding] = &[
    bind(ch('q'), Action::RequestQuit),
    bind(KeyCode::Esc, Action::Escape),
];

pub const HELP: &[Binding] = &[
    bind(ch('q'), Action::RequestQuit),
    bind(KeyCode::Esc, Action::Escape),
    bind(ch('?'), Action::Escape),
];

/// Tables consulted, in order, for `mode`.
pub fn tables(mode: InputMode) -> &'static [&'static [Binding]] {
    match mode {
        InputMode::Normal => &[GLOBAL, PROCESSES],
        InputMode::Services => &[GLOBAL, SERVICES],
        InputMode::Logs => &[GLOBAL, LOGS],
        InputMode::Network => &[GLOBAL, NETWORK],
        InputMode::ProcessSearch => &[PROCESS_SEARCH],
        InputMode::ServiceSearch => &[SERVICE_SEARCH],
        InputMode::LogSearch => &[LOG_SEARCH],
        InputMode::ProcessSignalConfirm => &[SIGNAL_CONFIRM],
        InputMode::Menu => &[MENU],
        InputMode::Help => &[HELP],
        InputMode::About
        | InputMode::ProcessDetail
        | InputMode::ServiceDetail
        | InputMode::LogDetail
        | InputMode::NetworkDetail => &[POPUP],
    }
}

/// The action of the first binding in `table` that matches `key`.
/// The character key bound to `action` in `table`, for hints that name it.
pub fn key_for(table: &[Binding], action: Action) -> Option<char> {
    table.iter().find_map(|binding| match binding.key.code {
        KeyCode::Char(key) if binding.action == action => Some(key),
        _ => None,
    })
}

pub fn find(table: &[Binding], key: KeyEvent) -> Option<Action> {
    table
        .iter()
        .find(|binding| binding.key.matches(key))
        .map(|binding| binding.action.clone())
}

/// The action bound to `key` in `mode`, if any.
pub fn lookup(mode: InputMode, key: KeyEvent) -> Option<Action> {
    tables(mode).iter().find_map(|table| find(table, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_can_name_the_key_of_an_action() {
        assert_eq!(key_for(PROCESSES, Action::TogglePin), Some('P'));
        assert_eq!(key_for(PROCESSES, Action::Quit), None);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn modifier_bindings_take_precedence_over_plain_keys() {
        let mode = InputMode::Normal;
        assert_eq!(
            lookup(mode, key(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(Action::PreviousTab)
        );
        assert_eq!(
            lookup(mode, key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(Action::NextTab)
        );
        assert_eq!(
            lookup(mode, key(KeyCode::Up, KeyModifiers::ALT)),
            Some(Action::MoveSelectedPin(PinMove::Up))
        );
        assert_eq!(
            lookup(mode, key(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::ProcessPrevious)
        );
        assert_eq!(
            lookup(mode, key(ch('k'), KeyModifiers::SHIFT)),
            Some(Action::RequestProcessSignal(ProcessSignal::Kill))
        );
        assert_eq!(
            lookup(mode, key(ch('k'), KeyModifiers::NONE)),
            Some(Action::ProcessPrevious)
        );
    }

    #[test]
    fn plain_bindings_ignore_modifiers() {
        assert_eq!(
            lookup(InputMode::Normal, key(ch('c'), KeyModifiers::ALT)),
            Some(Action::SortProcesses(ProcessSortField::Cpu))
        );
    }

    /// Actions that change the system or persistent state.
    fn changes_state(action: &Action) -> bool {
        matches!(
            action,
            Action::RequestProcessSignal(_) | Action::TogglePin | Action::MoveSelectedPin(_)
        )
    }

    #[test]
    fn state_changing_actions_need_shift_or_an_uppercase_key() {
        let all = [
            GLOBAL,
            PROCESSES,
            SERVICES,
            LOGS,
            NETWORK,
            PROCESS_SEARCH,
            SERVICE_SEARCH,
            LOG_SEARCH,
            SIGNAL_CONFIRM,
            MENU,
            POPUP,
            HELP,
        ];
        for binding in all.iter().flat_map(|table| table.iter()) {
            if !changes_state(&binding.action) {
                continue;
            }
            let shifted = match binding.key.mods {
                Mods::AnyOf(mods) => mods.contains(KeyModifiers::SHIFT),
                Mods::Any => {
                    matches!(binding.key.code, KeyCode::Char(c) if c.is_ascii_uppercase())
                }
            };
            assert!(shifted, "{binding:?} changes state without Shift");
        }
    }
}
