//! Terminal events to `Action`s: keys through the key-binding tables, mouse
//! clicks and the wheel through the hit regions of the last frame, and a tick
//! every tick period whether or not input arrives. Mouse movement produces an
//! action only when the element under the pointer changes, so hovering does
//! not cost redraws.

use std::{
    io,
    time::{Duration, Instant},
};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use crate::{
    action::{Action, InputMode, MouseTarget},
    keymap,
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
                let target = regions.hover_target_at(mouse.column, mouse.row);
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
        MouseTarget::PinMove(identity, direction) => Action::MovePin(identity, direction),
        MouseTarget::ProcessSignalCancel => Action::CancelProcessSignal,
        MouseTarget::ProcessSignalConfirm => Action::ConfirmProcessSignal,
        MouseTarget::MenuItem(item) => Action::ActivateMenuItem(item),
        MouseTarget::IntervalStep(step) => Action::StepSamplingInterval(step),
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

    if let Some(action) = keymap::lookup(input_mode, key) {
        return Some(action);
    }

    // Unbound characters are query text while searching.
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match input_mode {
        InputMode::ProcessSearch => Some(Action::AppendProcessSearch(character)),
        InputMode::ServiceSearch => Some(Action::AppendServiceSearch(character)),
        InputMode::LogSearch => Some(Action::AppendLogSearch(character)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
