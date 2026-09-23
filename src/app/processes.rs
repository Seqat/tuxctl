//! Processes screen: snapshot handling, search, sorting, selection, pinning and
//! signal confirmation.

use super::*;

/// Upper bound on pinned processes; keeps the pinned section within a small
/// terminal and pin lookups trivially cheap.
pub(super) const MAX_PINNED_PROCESSES: usize = 8;
/// How long an exited pinned process stays listed as `exited`.
pub(super) const EXITED_PIN_LINGER: Duration = Duration::from_secs(5);

/// A process the user pinned to the top of the table, identified by
/// `(pid, start_time)` so a reused PID never inherits the pin.
#[derive(Debug)]
pub(super) struct PinnedProcess {
    identity: ProcessIdentity,
    exited: Option<ExitedPin>,
}

#[derive(Debug)]
struct ExitedPin {
    /// The process as last seen, shown dimmed until the pin is dropped.
    last_seen: ProcessInfo,
    /// Set by the first tick after the exit was noticed.
    since: Option<Instant>,
}

/// One row of the Processes table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessRow {
    /// Index into `processes`. `dimmed` marks a pinned process that does not
    /// match the search; it stays listed but is never a fallback selection.
    Live { index: usize, dimmed: bool },
    /// Index into `pinned` of a pinned process that has exited.
    Exited { pin: usize },
}

/// What the table needs to draw one row.
#[derive(Debug, Clone, Copy)]
pub struct ProcessRowView<'a> {
    pub process: &'a ProcessInfo,
    pub pinned: bool,
    pub dimmed: bool,
    pub exited: bool,
}

impl App {
    /// Rows in the Processes table, including pinned rows.
    pub fn process_count(&self) -> usize {
        self.filtered_processes.len()
    }

    /// Live processes the table lists as matches (not dimmed, not exited).
    pub fn listed_process_count(&self) -> usize {
        self.filtered_processes
            .iter()
            .filter(|row| matches!(row, ProcessRow::Live { dimmed: false, .. }))
            .count()
    }

    /// Leading rows that belong to the pinned section.
    pub fn pinned_row_count(&self) -> usize {
        self.filtered_processes
            .iter()
            .take_while(|row| self.row_is_pinned(**row))
            .count()
    }

    pub fn process_summary(&self) -> ProcessSummary {
        self.process_summary
    }

    /// The process shown at `index`; for an exited pinned row, as last seen.
    pub fn process_at(&self, index: usize) -> Option<&ProcessInfo> {
        self.row_process(*self.filtered_processes.get(index)?)
    }

    pub fn process_row_at(&self, index: usize) -> Option<ProcessRowView<'_>> {
        let row = *self.filtered_processes.get(index)?;
        Some(ProcessRowView {
            process: self.row_process(row)?,
            pinned: self.row_is_pinned(row),
            dimmed: matches!(row, ProcessRow::Live { dimmed: true, .. }),
            exited: matches!(row, ProcessRow::Exited { .. }),
        })
    }

    pub fn selected_process_index(&self) -> Option<usize> {
        self.row_position(self.selected_process?)
    }

    fn row_process(&self, row: ProcessRow) -> Option<&ProcessInfo> {
        match row {
            ProcessRow::Live { index, .. } => self.processes.get(index),
            ProcessRow::Exited { pin } => self
                .pinned
                .get(pin)
                .and_then(|pin| pin.exited.as_ref())
                .map(|exited| &exited.last_seen),
        }
    }

    fn row_is_pinned(&self, row: ProcessRow) -> bool {
        match row {
            ProcessRow::Live { index, .. } => self.processes.get(index).is_some_and(|process| {
                self.pinned
                    .iter()
                    .any(|pin| pin.identity == process.identity())
            }),
            ProcessRow::Exited { .. } => true,
        }
    }

    /// Row index of `identity` in the table, pinned or not.
    fn row_position(&self, identity: ProcessIdentity) -> Option<usize> {
        self.filtered_processes.iter().position(|row| {
            self.row_process(*row)
                .is_some_and(|process| process.identity() == identity)
        })
    }

    /// The selected row's identity, also when it is an exited pinned process.
    pub fn selected_process_identity(&self) -> Option<ProcessIdentity> {
        self.selected_process
    }

    /// The selected process if it is still running.
    pub fn selected_process(&self) -> Option<&ProcessInfo> {
        let selected = self.selected_process?;
        self.processes
            .iter()
            .find(|process| process.identity() == selected)
    }

    pub fn process_scroll(&self) -> usize {
        self.process_scroll
    }

    pub fn process_search_query(&self) -> &str {
        &self.process_search_query
    }

    pub fn process_searching(&self) -> bool {
        self.process_searching
    }

    pub fn process_detail_visible(&self) -> bool {
        self.overlay == Some(Overlay::ProcessDetail)
    }

    pub fn process_error(&self) -> Option<&str> {
        self.process_error.as_deref()
    }

    pub fn process_sort(&self) -> ProcessSort {
        self.process_sort
    }

    pub fn process_signal_confirmation(&self) -> Option<&ProcessSignalConfirmation> {
        match &self.overlay {
            Some(Overlay::ProcessSignal(confirmation)) => Some(confirmation),
            _ => None,
        }
    }

    /// Removes and returns the signal confirmation, leaving any other overlay open.
    fn take_process_signal_confirmation(&mut self) -> Option<ProcessSignalConfirmation> {
        match self.overlay.take() {
            Some(Overlay::ProcessSignal(confirmation)) => Some(confirmation),
            other => {
                self.overlay = other;
                None
            }
        }
    }

    pub fn process_action_message(&self) -> Option<&str> {
        self.process_action_message.as_deref()
    }

    pub(super) fn update_processes(&mut self, snapshot: ProcessSnapshot) -> bool {
        let processes_visible = self.active_tab == Tab::Processes;
        let overview_visible = self.active_tab == Tab::Overview;
        let was_stale = self.process_error.is_some();
        let summary = snapshot.summary();
        let overview_summary_changed = overview_visible && self.process_summary != summary;
        if let Some(error) = snapshot.error {
            if self.process_error.as_ref() == Some(&error) {
                return false;
            }
            self.process_error = Some(error);
            return processes_visible || overview_visible;
        }

        if self.process_error.is_none() && self.processes == snapshot.processes {
            return false;
        }

        if !processes_visible {
            // Filtering and sorting are only needed to show the table, so hidden
            // snapshots defer them until Processes is selected again. The cleared
            // index list keeps stale indices from pointing into the new Vec.
            if self.deferred_process_rebuild.is_none() {
                self.deferred_process_rebuild = Some(self.selected_process_index().unwrap_or(0));
            }
            self.filtered_processes.clear();
            self.mark_exited_pins(&snapshot.processes);
            self.processes = snapshot.processes;
            self.process_keys.clear();
            self.process_summary = summary;
            self.process_error = None;
            self.close_confirmation_for_exited_process();
            return overview_summary_changed || (overview_visible && was_stale);
        }

        let previous_index = self.selected_process_index().unwrap_or(0);
        let previous_selection = self.selected_process;
        self.mark_exited_pins(&snapshot.processes);
        self.processes = snapshot.processes;
        self.process_keys.clear();
        self.process_summary = summary;
        self.process_error = None;
        self.rebuild_process_filter();

        self.selected_process =
            previous_selection.filter(|identity| self.row_position(*identity).is_some());

        self.close_confirmation_for_exited_process();

        if self.selected_process.is_none() {
            self.selected_process = self.fallback_process_selection(previous_index);
            self.close_overlay(&Overlay::ProcessDetail);
        } else if self.selected_process().is_none() {
            // The selected pinned process exited; its row stays, its details go.
            self.close_overlay(&Overlay::ProcessDetail);
        }
        self.reconcile_hovered_process();
        self.ensure_process_visible();
        processes_visible || overview_summary_changed || (overview_visible && was_stale)
    }

    fn close_confirmation_for_exited_process(&mut self) {
        if let Some(confirmation) = self.process_signal_confirmation() {
            if !self
                .processes
                .iter()
                .any(|p| p.identity() == confirmation.identity)
            {
                let name = confirmation.name.clone();
                let pid = confirmation.identity.pid;
                self.overlay = None;
                self.process_action_message =
                    Some(format!("Process {name} ({pid}) exited before signal"));
            }
        }
    }

    pub(super) fn begin_process_search(&mut self) -> bool {
        if self.active_tab == Tab::Processes {
            self.process_search_query.clear();
            self.process_searching = true;
            self.process_action_message = None;
            self.rebuild_process_filter();
            self.leave_dimmed_selection();
            true
        } else {
            false
        }
    }

    pub(super) fn append_process_search(&mut self, character: char) -> bool {
        if self.process_searching && !character.is_control() {
            self.process_search_query.push(character);
            self.rebuild_process_filter();
            self.leave_dimmed_selection();
            true
        } else {
            false
        }
    }

    pub(super) fn backspace_process_search(&mut self) -> bool {
        if self.process_searching && self.process_search_query.pop().is_some() {
            self.rebuild_process_filter();
            self.leave_dimmed_selection();
            true
        } else {
            false
        }
    }

    pub(super) fn rebuild_process_filter(&mut self) {
        let previous_index = match self.deferred_process_rebuild.take() {
            Some(index) => index,
            None => self.selected_process_index().unwrap_or(0),
        };
        let previous_selection = self.selected_process;
        let query = self.process_search_query.to_lowercase();
        if self.process_keys.len() != self.processes.len() {
            self.process_keys = self.processes.iter().map(ProcessKeys::new).collect();
        }

        let keys = &self.process_keys;
        let processes = &self.processes;
        let pinned = &self.pinned;
        // One pass: live pinned processes go to their slot (at most
        // MAX_PINNED_PROCESSES identity comparisons each), the rest are filtered.
        let mut pinned_rows = [None; MAX_PINNED_PROCESSES];
        let mut rows = std::mem::take(&mut self.filtered_processes);
        rows.clear();
        for (index, (process, keys)) in processes.iter().zip(keys).enumerate() {
            let matches = process_matches(process, keys, &query);
            let identity = process.identity();
            if let Some(slot) = pinned.iter().position(|pin| pin.identity == identity) {
                if let Some(row) = pinned_rows.get_mut(slot) {
                    *row = Some(ProcessRow::Live {
                        index,
                        dimmed: !matches,
                    });
                }
            } else if matches {
                rows.push(ProcessRow::Live {
                    index,
                    dimmed: false,
                });
            }
        }
        let sort = self.process_sort;
        let live_index = |row: &ProcessRow| match *row {
            ProcessRow::Live { index, .. } => index,
            ProcessRow::Exited { .. } => unreachable!("unpinned rows are live"),
        };
        rows.sort_by(|left, right| {
            let (left, right) = (live_index(left), live_index(right));
            compare_processes(
                (&processes[left], &keys[left]),
                (&processes[right], &keys[right]),
                sort,
            )
        });
        // Pinned rows lead in the user's order; sorting never touches them.
        let pinned_section = pinned.iter().enumerate().filter_map(|(slot, pin)| {
            if pin.exited.is_some() {
                Some(ProcessRow::Exited { pin: slot })
            } else {
                pinned_rows.get(slot).copied().flatten()
            }
        });
        rows.splice(0..0, pinned_section);
        self.filtered_processes = rows;

        self.selected_process =
            previous_selection.filter(|identity| self.row_position(*identity).is_some());
        if self.selected_process.is_none() {
            self.selected_process = self.fallback_process_selection(previous_index);
        }
        self.reconcile_hovered_process();
        self.ensure_process_visible();
    }

    pub(super) fn move_process_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Processes || self.filtered_processes.is_empty() {
            return false;
        }

        let current = self.selected_process_index().unwrap_or(0);
        let last = self.filtered_processes.len() - 1;
        let next = current.saturating_add_signed(delta).min(last);
        self.select_process_index(next)
    }

    pub(super) fn select_process_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Processes {
            return false;
        }

        if let Some(process) = self.process_at(index) {
            let previous = (self.selected_process, self.process_scroll);
            self.selected_process = Some(process.identity());
            self.ensure_process_visible();
            previous != (self.selected_process, self.process_scroll)
        } else {
            false
        }
    }

    pub(super) fn select_process(&mut self, identity: ProcessIdentity) -> bool {
        if self.active_tab != Tab::Processes {
            return false;
        }

        if let Some(index) = self.row_position(identity) {
            self.select_process_index(index)
        } else {
            false
        }
    }

    pub(super) fn open_process_details(&mut self) -> bool {
        if self.active_tab == Tab::Processes && self.selected_process().is_some() {
            self.process_searching = false;
            self.overlay = Some(Overlay::ProcessDetail);
            self.hovered = None;
            true
        } else {
            false
        }
    }

    pub(super) fn request_process_signal(&mut self, signal: ProcessSignal) -> bool {
        if self.active_tab != Tab::Processes {
            return false;
        }
        let (identity, name) = {
            let Some(selected) = self.selected_process() else {
                return false;
            };
            (selected.identity(), selected.name.clone())
        };
        self.process_searching = false;
        self.process_action_message = None;
        self.overlay = Some(Overlay::ProcessSignal(ProcessSignalConfirmation {
            identity,
            name,
            signal,
            focused_button: SignalConfirmButton::Cancel,
        }));
        self.hovered = None;
        true
    }

    pub(super) fn cancel_process_signal(&mut self) -> bool {
        if self.take_process_signal_confirmation().is_some() {
            self.hovered = None;
            true
        } else {
            false
        }
    }

    pub(super) fn toggle_process_signal_focus(&mut self) -> bool {
        if let Some(Overlay::ProcessSignal(confirmation)) = &mut self.overlay {
            confirmation.focused_button = match confirmation.focused_button {
                SignalConfirmButton::Cancel => SignalConfirmButton::Confirm,
                SignalConfirmButton::Confirm => SignalConfirmButton::Cancel,
            };
            true
        } else {
            false
        }
    }

    pub(super) fn focus_process_signal(&mut self, button: SignalConfirmButton) -> bool {
        if let Some(Overlay::ProcessSignal(confirmation)) = &mut self.overlay {
            if confirmation.focused_button != button {
                confirmation.focused_button = button;
                return true;
            }
        }
        false
    }

    pub(super) fn execute_focused_process_signal(&mut self) -> bool {
        let Some(confirmation) = self.process_signal_confirmation() else {
            return false;
        };
        match confirmation.focused_button {
            SignalConfirmButton::Cancel => self.cancel_process_signal(),
            SignalConfirmButton::Confirm => self.confirm_process_signal(),
        }
    }

    pub(super) fn confirm_process_signal(&mut self) -> bool {
        let Some(confirmation) = self.take_process_signal_confirmation() else {
            return false;
        };
        let result = send_process_signal(confirmation.identity, confirmation.signal);
        self.finish_process_signal(confirmation, result)
    }

    #[cfg(test)]
    pub(crate) fn confirm_process_signal_at<H, O, S>(
        &mut self,
        proc_dir: &std::path::Path,
        open_pidfd: O,
        send_signal: S,
    ) -> bool
    where
        O: FnOnce(libc::pid_t) -> std::io::Result<H>,
        S: FnOnce(&H, libc::c_int) -> std::io::Result<()>,
    {
        let Some(confirmation) = self.take_process_signal_confirmation() else {
            return false;
        };
        let result = verify_and_send_signal_at(
            proc_dir,
            confirmation.identity,
            confirmation.signal,
            open_pidfd,
            send_signal,
        );
        self.finish_process_signal(confirmation, result)
    }

    fn finish_process_signal(
        &mut self,
        confirmation: ProcessSignalConfirmation,
        result: Result<(), ProcessSignalError>,
    ) -> bool {
        self.hovered = None;
        let sig_name = confirmation.signal.name();
        match result {
            Ok(()) => {
                self.process_action_message = Some(format!(
                    "Sent {} to {} ({})",
                    sig_name, confirmation.name, confirmation.identity.pid
                ));
            }
            Err(ProcessSignalError::ProcessNotFound) => {
                self.process_action_message = Some(format!(
                    "Failed to send {}: process {} ({}) not found",
                    sig_name, confirmation.name, confirmation.identity.pid
                ));
            }
            Err(ProcessSignalError::StaleIdentity) => {
                self.process_action_message = Some(format!(
                    "Refused to send {}: PID {} was reused",
                    sig_name, confirmation.identity.pid
                ));
            }
            Err(ProcessSignalError::PermissionDenied) => {
                self.process_action_message = Some(format!(
                    "Failed to send {}: permission denied for {} ({})",
                    sig_name, confirmation.name, confirmation.identity.pid
                ));
            }
            Err(ProcessSignalError::Unsupported) => {
                self.process_action_message = Some(format!(
                    "Failed to send {}: pidfd signaling is not supported",
                    sig_name
                ));
            }
            Err(ProcessSignalError::Failed(err)) => {
                self.process_action_message = Some(format!(
                    "Failed to send {} to {} ({}): {}",
                    sig_name, confirmation.name, confirmation.identity.pid, err
                ));
            }
        }
        true
    }

    pub(super) fn sort_processes(&mut self, field: ProcessSortField) -> bool {
        if self.active_tab != Tab::Processes {
            return false;
        }

        if self.process_sort.field == field {
            self.process_sort.descending = !self.process_sort.descending;
        } else {
            self.process_sort = ProcessSort::for_field(field);
        }
        self.rebuild_process_filter();
        true
    }

    /// The row to select when the selection is gone: the row now at
    /// `previous_index`, or the nearest matching live row after or before it.
    /// Dimmed and exited pinned rows are only ever selected on purpose.
    fn fallback_process_selection(&self, previous_index: usize) -> Option<ProcessIdentity> {
        let last = self.filtered_processes.len().checked_sub(1)?;
        let start = previous_index.min(last);
        let selectable = |index: &usize| {
            matches!(
                self.filtered_processes[*index],
                ProcessRow::Live { dimmed: false, .. }
            )
        };
        (start..=last)
            .find(selectable)
            .or_else(|| (0..start).rev().find(selectable))
            .and_then(|index| self.process_at(index))
            .map(ProcessInfo::identity)
    }

    /// After a query edit, moves the selection off a pinned row the query no
    /// longer matches, so Enter acts on a match. Refreshes do not call this:
    /// a dimmed row the user moved to on purpose stays selected.
    fn leave_dimmed_selection(&mut self) {
        let Some(index) = self.selected_process_index() else {
            return;
        };
        if matches!(
            self.filtered_processes[index],
            ProcessRow::Live { dimmed: true, .. }
        ) {
            self.selected_process = self.fallback_process_selection(index);
            self.ensure_process_visible();
        }
    }

    /// Marks pinned processes missing from `next` as exited, keeping their
    /// last known data for display. Only called with a successful snapshot.
    fn mark_exited_pins(&mut self, next: &[ProcessInfo]) {
        let processes = &self.processes;
        self.pinned.retain_mut(|pin| {
            if pin.exited.is_some() || next.iter().any(|p| p.identity() == pin.identity) {
                return true;
            }
            match processes.iter().find(|p| p.identity() == pin.identity) {
                Some(last_seen) => {
                    pin.exited = Some(ExitedPin {
                        last_seen: last_seen.clone(),
                        since: None,
                    });
                    true
                }
                // Never seen in this App's data: nothing to show.
                None => false,
            }
        });
    }

    /// Drops exited pins after [`EXITED_PIN_LINGER`]; redraws only when a
    /// visible row goes away.
    pub(super) fn expire_exited_pins(&mut self, now: Instant) -> bool {
        let before = self.pinned.len();
        self.pinned.retain_mut(|pin| match &mut pin.exited {
            None => true,
            Some(exited) => {
                let since = *exited.since.get_or_insert(now);
                now.saturating_duration_since(since) < EXITED_PIN_LINGER
            }
        });
        if self.pinned.len() == before {
            return false;
        }
        // Exited rows refer to pins by position, so the rows must be rebuilt.
        self.rebuild_process_filter();
        self.active_tab == Tab::Processes
    }

    pub(super) fn toggle_selected_pin(&mut self) -> bool {
        if self.active_tab != Tab::Processes {
            return false;
        }
        let Some(identity) = self.selected_process else {
            return false;
        };
        if let Some(position) = self.pinned.iter().position(|pin| pin.identity == identity) {
            self.pinned.remove(position);
        } else {
            if self.selected_process().is_none() {
                return false;
            }
            if self.pinned.len() >= MAX_PINNED_PROCESSES {
                self.process_action_message =
                    Some(format!("Pin limit ({MAX_PINNED_PROCESSES}) reached"));
                return true;
            }
            self.pinned.push(PinnedProcess {
                identity,
                exited: None,
            });
        }
        self.rebuild_process_filter();
        true
    }

    pub(super) fn move_selected_pin(&mut self, direction: PinMove) -> bool {
        match self.selected_process {
            Some(identity) => self.move_pin(identity, direction),
            None => false,
        }
    }

    /// Moves a pinned process one place within the pinned section; the
    /// selection stays on whatever it was on.
    pub(super) fn move_pin(&mut self, identity: ProcessIdentity, direction: PinMove) -> bool {
        if self.active_tab != Tab::Processes {
            return false;
        }
        let Some(position) = self.pinned.iter().position(|pin| pin.identity == identity) else {
            return false;
        };
        let target = match direction {
            PinMove::Up => position.checked_sub(1),
            PinMove::Down => Some(position + 1).filter(|target| *target < self.pinned.len()),
        };
        let Some(target) = target else {
            return false;
        };
        self.pinned.swap(position, target);
        self.rebuild_process_filter();
        true
    }

    pub(super) fn ensure_process_visible(&mut self) {
        self.process_scroll = calculate_scroll(
            self.filtered_processes.len(),
            self.selected_process_index(),
            self.process_scroll,
            self.process_view_height,
        );
    }

    fn reconcile_hovered_process(&mut self) {
        if let Some(MouseTarget::ProcessRow(identity)) = &self.hovered {
            if self.row_position(*identity).is_none() {
                self.hovered = None;
            }
        }
    }
}

/// Lowercased fields used for case-insensitive search and name sorting, computed
/// once per snapshot instead of once per comparison.
#[derive(Debug)]
pub(super) struct ProcessKeys {
    name: Box<str>,
    command: Option<Box<str>>,
}

impl ProcessKeys {
    fn new(process: &ProcessInfo) -> Self {
        Self {
            name: process.name.to_lowercase().into(),
            command: process
                .command
                .as_deref()
                .map(|command| command.to_lowercase().into()),
        }
    }
}

fn compare_processes(
    (left, left_keys): (&ProcessInfo, &ProcessKeys),
    (right, right_keys): (&ProcessInfo, &ProcessKeys),
    sort: ProcessSort,
) -> Ordering {
    let order = match sort.field {
        ProcessSortField::Cpu => {
            compare_optional_cpu(left.cpu_percent, right.cpu_percent, sort.descending)
        }
        ProcessSortField::Memory => {
            ordered(left.memory_bytes.cmp(&right.memory_bytes), sort.descending)
        }
        ProcessSortField::Pid => ordered(left.pid.cmp(&right.pid), sort.descending),
        ProcessSortField::Name => ordered(left_keys.name.cmp(&right_keys.name), sort.descending),
    };

    order.then_with(|| left.pid.cmp(&right.pid))
}

fn compare_optional_cpu(left: Option<f64>, right: Option<f64>, descending: bool) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => ordered(left.total_cmp(&right), descending),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn ordered(order: Ordering, descending: bool) -> Ordering {
    if descending {
        order.reverse()
    } else {
        order
    }
}

fn process_matches(process: &ProcessInfo, keys: &ProcessKeys, query: &str) -> bool {
    query.is_empty()
        || keys.name.contains(query)
        || process.pid.to_string().contains(query)
        || keys
            .command
            .as_deref()
            .is_some_and(|command| command.contains(query))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn process_filter_is_case_insensitive() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(10, "Firefox"),
            process(20, "postgres"),
        ])));

        app.update(Action::BeginProcessSearch);
        for character in "FIRE".chars() {
            app.update(Action::AppendProcessSearch(character));
        }

        assert_eq!(app.process_count(), 1);
        assert_eq!(app.process_at(0).map(|process| process.pid), Some(10));
    }

    #[test]
    fn cpu_sort_defaults_to_descending_and_toggles_to_ascending() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process_with(1, "one", Some(5.0), 100),
            process_with(2, "two", Some(20.0), 200),
            process_with(3, "three", None, 300),
        ])));

        assert_eq!(visible_pids(&app), vec![2, 1, 3]);

        app.update(Action::SortProcesses(ProcessSortField::Cpu));
        assert_eq!(visible_pids(&app), vec![1, 2, 3]);
        assert!(!app.process_sort().descending);
    }

    #[test]
    fn memory_sort_uses_descending_then_ascending_order() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process_with(1, "one", Some(1.0), 100),
            process_with(2, "two", Some(1.0), 300),
            process_with(3, "three", Some(1.0), 200),
        ])));

        app.update(Action::SortProcesses(ProcessSortField::Memory));
        assert_eq!(visible_pids(&app), vec![2, 3, 1]);

        app.update(Action::SortProcesses(ProcessSortField::Memory));
        assert_eq!(visible_pids(&app), vec![1, 3, 2]);
    }

    #[test]
    fn pid_sort_uses_ascending_then_descending_order() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(20, "twenty"),
            process(3, "three"),
            process(11, "eleven"),
        ])));

        app.update(Action::SortProcesses(ProcessSortField::Pid));
        assert_eq!(visible_pids(&app), vec![3, 11, 20]);

        app.update(Action::SortProcesses(ProcessSortField::Pid));
        assert_eq!(visible_pids(&app), vec![20, 11, 3]);
    }

    #[test]
    fn name_sort_is_case_insensitive_and_toggleable() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "zebra"),
            process(2, "Alpha"),
            process(3, "middle"),
        ])));

        app.update(Action::SortProcesses(ProcessSortField::Name));
        assert_eq!(visible_pids(&app), vec![2, 3, 1]);

        app.update(Action::SortProcesses(ProcessSortField::Name));
        assert_eq!(visible_pids(&app), vec![1, 3, 2]);
    }

    #[test]
    fn filtering_happens_before_sorting() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process_with(1, "alpha-low", Some(1.0), 100),
            process_with(2, "unrelated", Some(50.0), 500),
            process_with(3, "ALPHA-high", Some(10.0), 300),
        ])));
        app.update(Action::SortProcesses(ProcessSortField::Memory));
        app.update(Action::BeginProcessSearch);
        for character in "alpha".chars() {
            app.update(Action::AppendProcessSearch(character));
        }

        assert_eq!(visible_pids(&app), vec![3, 1]);
    }

    #[test]
    fn sort_and_refresh_preserve_selected_process_identity() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process_with(1, "zulu", Some(5.0), 100),
            process_with(2, "alpha", Some(10.0), 200),
        ])));
        app.update(Action::SelectProcess(ProcessIdentity {
            pid: 1,
            start_time: 1,
        }));

        app.update(Action::SortProcesses(ProcessSortField::Name));
        assert_eq!(
            app.selected_process().map(ProcessInfo::identity),
            Some(ProcessIdentity {
                pid: 1,
                start_time: 1
            })
        );

        app.update(Action::ProcessesUpdated(processes(vec![
            process_with(2, "alpha", Some(1.0), 200),
            process_with(1, "zulu", Some(99.0), 100),
        ])));
        assert_eq!(
            app.selected_process().map(ProcessInfo::identity),
            Some(ProcessIdentity {
                pid: 1,
                start_time: 1
            })
        );
    }

    #[test]
    fn selection_remains_visible_while_moving_and_paging() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(
            (1..=10)
                .map(|pid| process(pid, &format!("process-{pid}")))
                .collect(),
        )));
        app.update(Action::ProcessViewportChanged {
            start: 0,
            height: 3,
        });

        app.update(Action::ProcessNextPage);
        assert_eq!(app.selected_process_index(), Some(3));
        assert_eq!(app.process_scroll(), 1);

        app.update(Action::ProcessLast);
        assert_eq!(app.selected_process_index(), Some(9));
        assert_eq!(app.process_scroll(), 7);
    }

    #[test]
    fn disappearing_selection_uses_nearest_row_and_closes_details() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "one"),
            process(2, "two"),
            process(3, "three"),
        ])));
        app.update(Action::SelectProcess(ProcessIdentity {
            pid: 2,
            start_time: 2,
        }));
        app.update(Action::OpenProcessDetails);

        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "one"),
            process(3, "three"),
        ])));

        assert_eq!(app.selected_process().map(|process| process.pid), Some(3));
        assert!(!app.process_detail_visible());
    }

    #[test]
    fn reused_pid_does_not_keep_stale_detail_selection() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(7, "old")])));
        app.update(Action::OpenProcessDetails);

        let mut replacement = process(7, "new");
        replacement.start_time = 999;
        app.update(Action::ProcessesUpdated(processes(vec![replacement])));

        assert_eq!(
            app.selected_process().map(|process| process.name.as_str()),
            Some("new")
        );
        assert!(!app.process_detail_visible());
    }

    #[test]
    fn process_refresh_clears_only_a_stale_hover_identity() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(7, "old")])));
        let old_identity = app.process_at(0).unwrap().identity();
        app.update(Action::HoverMouseTarget(Some(MouseTarget::ProcessRow(
            old_identity,
        ))));

        let mut replacement = process(7, "new");
        replacement.start_time = 999;
        app.update(Action::ProcessesUpdated(processes(vec![replacement])));

        assert_eq!(app.hovered(), None);
    }

    #[test]
    fn hover_does_not_rebuild_or_select_the_process_list() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "one"),
            process(2, "two"),
        ])));
        let selected = app.selected_process().map(ProcessInfo::identity);
        let order = visible_pids(&app);
        let hovered = app.process_at(1).unwrap().identity();

        app.update(Action::HoverMouseTarget(Some(MouseTarget::ProcessRow(
            hovered,
        ))));

        assert_eq!(app.selected_process().map(ProcessInfo::identity), selected);
        assert_eq!(visible_pids(&app), order);
    }

    #[test]
    fn process_signal_confirmation_workflow() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(100, "bash"),
            process(200, "nginx"),
        ])));

        assert_eq!(app.selected_process().map(|p| p.pid), Some(100));

        // 1. Request SIGTERM
        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        let confirmation = app
            .process_signal_confirmation()
            .expect("confirmation open");
        assert_eq!(confirmation.identity.pid, 100);
        assert_eq!(confirmation.name, "bash");
        assert_eq!(confirmation.signal, ProcessSignal::Term);
        assert_eq!(confirmation.focused_button, SignalConfirmButton::Cancel);
        assert_eq!(app.input_mode(), InputMode::ProcessSignalConfirm);

        // 2. Tab/toggle focus between Cancel and Confirm
        app.update(Action::ToggleProcessSignalFocus);
        assert_eq!(
            app.process_signal_confirmation().unwrap().focused_button,
            SignalConfirmButton::Confirm
        );
        app.update(Action::ToggleProcessSignalFocus);
        assert_eq!(
            app.process_signal_confirmation().unwrap().focused_button,
            SignalConfirmButton::Cancel
        );

        // Explicit button focus
        app.update(Action::FocusProcessSignal(SignalConfirmButton::Confirm));
        assert_eq!(
            app.process_signal_confirmation().unwrap().focused_button,
            SignalConfirmButton::Confirm
        );
        app.update(Action::FocusProcessSignal(SignalConfirmButton::Cancel));
        assert_eq!(
            app.process_signal_confirmation().unwrap().focused_button,
            SignalConfirmButton::Cancel
        );

        // 3. Enter on Cancel (default) cancels the modal
        app.update(Action::ExecuteFocusedProcessSignal);
        assert!(app.process_signal_confirmation().is_none());
        assert_eq!(app.input_mode(), InputMode::Normal);
        assert!(app.process_action_message().is_none());

        // 4. Request SIGKILL
        app.update(Action::RequestProcessSignal(ProcessSignal::Kill));
        let confirmation = app
            .process_signal_confirmation()
            .expect("confirmation open");
        assert_eq!(confirmation.signal, ProcessSignal::Kill);
        assert_eq!(confirmation.focused_button, SignalConfirmButton::Cancel);

        // 5. Esc cancels the modal
        app.update(Action::Escape);
        assert!(app.process_signal_confirmation().is_none());

        // 6. Cancel action cancels the modal
        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        assert!(app.process_signal_confirmation().is_some());
        app.update(Action::CancelProcessSignal);
        assert!(app.process_signal_confirmation().is_none());

        // 7. Modal blocks tab switching until dismissed
        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        assert!(app.process_signal_confirmation().is_some());
        app.update(Action::NextTab);
        assert!(app.process_signal_confirmation().is_some());
        assert_eq!(app.active_tab(), Tab::Processes);
        app.update(Action::Escape);
        assert!(app.process_signal_confirmation().is_none());
    }

    #[test]
    fn process_signal_dismissed_if_target_process_exits() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(100, "bash"),
            process(200, "nginx"),
        ])));

        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        assert!(app.process_signal_confirmation().is_some());

        // Process 100 terminates/disappears in next snapshot
        app.update(Action::ProcessesUpdated(processes(vec![process(
            200, "nginx",
        )])));

        assert!(app.process_signal_confirmation().is_none());
        assert_eq!(
            app.process_action_message(),
            Some("Process bash (100) exited before signal")
        );
    }

    #[test]
    fn process_signal_verification_with_mock_proc() {
        let temp_dir = std::env::temp_dir().join(format!("tuxctl_app_test_{}", std::process::id()));
        let proc_100 = temp_dir.join("100");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&proc_100).unwrap();

        // Valid stat matching start_time = 100
        let stat_valid = "100 (bash) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        std::fs::write(proc_100.join("stat"), stat_valid).unwrap();

        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(
            100, "bash",
        )])));

        // Success case
        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        let mut signal_received = None;
        app.confirm_process_signal_at(&temp_dir, Ok, |pidfd, sig| {
            signal_received = Some((*pidfd, sig));
            Ok(())
        });

        assert_eq!(signal_received, Some((100, libc::SIGTERM)));
        assert_eq!(
            app.process_action_message(),
            Some("Sent SIGTERM to bash (100)")
        );

        // Stale identity (PID reused with start_time = 999)
        let stat_reused = "100 (bash) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        std::fs::write(proc_100.join("stat"), stat_reused).unwrap();

        app.update(Action::RequestProcessSignal(ProcessSignal::Kill));
        let mut signal_called_stale = false;
        app.confirm_process_signal_at(
            &temp_dir,
            |_| Ok(()),
            |_, _| {
                signal_called_stale = true;
                Ok(())
            },
        );

        assert!(
            !signal_called_stale,
            "Signal must NOT be sent when start_time differs"
        );
        assert_eq!(
            app.process_action_message(),
            Some("Refused to send SIGKILL: PID 100 was reused")
        );

        // Process not found (PID vanished)
        let _ = std::fs::remove_dir_all(&temp_dir);

        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        let mut signal_called_missing = false;
        app.confirm_process_signal_at(
            &temp_dir,
            |_| Ok(()),
            |_, _| {
                signal_called_missing = true;
                Ok(())
            },
        );

        assert!(!signal_called_missing);
        assert_eq!(
            app.process_action_message(),
            Some("Failed to send SIGTERM: process bash (100) not found")
        );

        // Unsupported pidfd syscalls are reported without a signal attempt.
        app.update(Action::RequestProcessSignal(ProcessSignal::Kill));
        let mut signal_called_unsupported = false;
        app.confirm_process_signal_at(
            &temp_dir,
            |_| Err::<(), _>(std::io::Error::from_raw_os_error(libc::ENOSYS)),
            |_, _| {
                signal_called_unsupported = true;
                Ok(())
            },
        );

        assert!(!signal_called_unsupported);
        assert_eq!(
            app.process_action_message(),
            Some("Failed to send SIGKILL: pidfd signaling is not supported")
        );
    }

    fn processes_tab_with(pids: &[u32]) -> App {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(
            pids.iter()
                .map(|&pid| process(pid, &format!("p{pid}")))
                .collect(),
        )));
        app.update(Action::SortProcesses(ProcessSortField::Pid));
        if app.process_sort().descending {
            app.update(Action::SortProcesses(ProcessSortField::Pid));
        }
        app
    }

    #[test]
    fn hidden_process_snapshots_defer_filtering_until_the_tab_returns() {
        let mut app = processes_tab_with(&[1, 2, 3]);
        app.update(Action::SelectTab(Tab::Overview));

        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "p1"),
            process(2, "p2"),
            process(3, "p3"),
            process(4, "p4"),
        ])));

        assert_eq!(app.process_count(), 0, "hidden snapshots skip the rebuild");
        assert_eq!(app.process_summary().total, 4);
        assert!(app.select_tab(Tab::Processes));
        assert_eq!(visible_pids(&app), vec![1, 2, 3, 4]);
    }

    #[test]
    fn selection_identity_survives_hidden_snapshots() {
        let mut app = processes_tab_with(&[1, 2, 3]);
        app.update(Action::SelectProcess(ProcessIdentity {
            pid: 2,
            start_time: 2,
        }));
        app.update(Action::SelectTab(Tab::Logs));

        app.update(Action::ProcessesUpdated(processes(vec![
            process(5, "p5"),
            process(2, "p2"),
            process(1, "p1"),
        ])));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(2, "p2"),
            process(1, "p1"),
            process(0, "p0"),
        ])));
        app.update(Action::SelectTab(Tab::Processes));

        assert_eq!(visible_pids(&app), vec![0, 1, 2]);
        assert_eq!(app.selected_process().map(|process| process.pid), Some(2));
        assert_eq!(app.selected_process_index(), Some(2));
    }

    #[test]
    fn exited_selection_falls_back_to_its_previous_row_after_hidden_snapshots() {
        let mut app = processes_tab_with(&[1, 2, 3, 4]);
        app.update(Action::SelectProcess(ProcessIdentity {
            pid: 2,
            start_time: 2,
        }));
        app.update(Action::SelectTab(Tab::Overview));

        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "p1"),
            process(3, "p3"),
            process(4, "p4"),
        ])));
        app.update(Action::SelectTab(Tab::Processes));

        assert_eq!(app.selected_process_index(), Some(1));
        assert_eq!(app.selected_process().map(|process| process.pid), Some(3));
    }

    #[test]
    fn shrinking_hidden_snapshot_never_leaves_out_of_bounds_rows() {
        let pids: Vec<u32> = (1..=10).collect();
        let mut app = processes_tab_with(&pids);
        app.update(Action::ProcessLast);
        app.update(Action::SelectTab(Tab::Network));

        app.update(Action::ProcessesUpdated(processes(vec![
            process(20, "p20"),
            process(21, "p21"),
        ])));
        for index in 0..10 {
            assert!(app.process_at(index).is_none());
        }
        app.update(Action::SelectTab(Tab::Processes));

        assert_eq!(visible_pids(&app), vec![20, 21]);
        assert_eq!(app.selected_process().map(|process| process.pid), Some(21));
    }

    #[test]
    fn active_search_applies_to_snapshots_received_while_hidden() {
        let mut app = processes_tab_with(&[1, 2]);
        app.update(Action::BeginProcessSearch);
        for character in "ssh".chars() {
            app.update(Action::AppendProcessSearch(character));
        }
        assert!(visible_pids(&app).is_empty());
        app.update(Action::SelectTab(Tab::Overview));

        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "p1"),
            process(7, "SSHD"),
            ProcessInfo {
                command: Some("/usr/bin/Agent --SSH-auth".into()),
                ..process(8, "agent")
            },
        ])));
        app.update(Action::SelectTab(Tab::Processes));

        assert_eq!(visible_pids(&app), vec![7, 8]);
        assert_eq!(app.process_search_query(), "ssh");
    }

    #[test]
    fn name_sort_is_case_insensitive_with_cached_keys() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "beta"),
            process(2, "Alpha"),
            process(3, "gamma"),
            process(4, "ALPHA"),
        ])));

        app.update(Action::SortProcesses(ProcessSortField::Name));
        if app.process_sort().descending {
            app.update(Action::SortProcesses(ProcessSortField::Name));
        }

        assert_eq!(visible_pids(&app), vec![2, 4, 1, 3]);
    }

    #[test]
    fn process_summary_is_cached_from_process_snapshots() {
        let mut app = App::default();
        let mut running = process(1, "running");
        running.state = "R (running)".into();
        running.state_code = 'R';
        let mut zombie = process(2, "zombie");
        zombie.state = "Z (zombie)".into();
        zombie.state_code = 'Z';

        assert!(app.update(Action::ProcessesUpdated(processes(vec![
            running,
            zombie,
            process(3, "sleeping"),
        ]))));
        assert_eq!(
            app.process_summary(),
            ProcessSummary {
                total: 3,
                running: 1,
                zombies: 1,
            }
        );
    }

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time: u64::from(pid),
        }
    }

    fn pin(app: &mut App, pid: u32) {
        app.update(Action::SelectProcess(identity(pid)));
        assert_eq!(app.selected_process_identity(), Some(identity(pid)));
        assert!(app.update(Action::TogglePin), "pin {pid}");
    }

    /// Processes 1..=count sorted by CPU descending: pid 1 lowest CPU.
    fn cpu_ranked(count: u32) -> Vec<ProcessInfo> {
        (1..=count)
            .map(|pid| process_with(pid, &format!("p{pid}"), Some(f64::from(pid)), 100))
            .collect()
    }

    fn processes_app(count: u32) -> App {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(cpu_ranked(count))));
        app
    }

    #[test]
    fn pinned_processes_lead_in_pin_order_without_duplicates() {
        let mut app = processes_app(5);
        assert_eq!(visible_pids(&app), vec![5, 4, 3, 2, 1]);

        pin(&mut app, 2);
        pin(&mut app, 4);

        assert_eq!(visible_pids(&app), vec![2, 4, 5, 3, 1]);
        assert_eq!(app.pinned_row_count(), 2);
        assert!(app.process_row_at(0).unwrap().pinned);
        assert!(!app.process_row_at(2).unwrap().pinned);
        assert_eq!(app.listed_process_count(), 5);
    }

    #[test]
    fn sorting_and_refresh_reorder_only_the_unpinned_section() {
        let mut app = processes_app(5);
        pin(&mut app, 1);
        pin(&mut app, 5);

        app.update(Action::SortProcesses(ProcessSortField::Pid));
        assert_eq!(visible_pids(&app), vec![1, 5, 2, 3, 4]);
        app.update(Action::SortProcesses(ProcessSortField::Pid));
        assert_eq!(visible_pids(&app), vec![1, 5, 4, 3, 2]);

        let mut refreshed = cpu_ranked(5);
        refreshed.reverse();
        app.update(Action::ProcessesUpdated(processes(refreshed)));
        assert_eq!(visible_pids(&app), vec![1, 5, 4, 3, 2]);

        // Hidden snapshots defer the rebuild; the pins survive it.
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::ProcessesUpdated(processes(cpu_ranked(6))));
        app.update(Action::SelectTab(Tab::Processes));
        assert_eq!(visible_pids(&app), vec![1, 5, 6, 4, 3, 2]);
    }

    #[test]
    fn unpinning_returns_a_process_to_its_sorted_place() {
        let mut app = processes_app(4);
        pin(&mut app, 1);
        assert_eq!(visible_pids(&app), vec![1, 4, 3, 2]);

        assert!(app.update(Action::TogglePin));

        assert_eq!(visible_pids(&app), vec![4, 3, 2, 1]);
        assert_eq!(app.pinned_row_count(), 0);
        assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
    }

    #[test]
    fn the_pin_limit_is_enforced_with_a_message() {
        let mut app = processes_app(MAX_PINNED_PROCESSES as u32 + 2);
        for pid in 1..=MAX_PINNED_PROCESSES as u32 {
            pin(&mut app, pid);
        }
        app.update(Action::SelectProcess(identity(
            MAX_PINNED_PROCESSES as u32 + 1,
        )));

        assert!(app.update(Action::TogglePin), "the message is shown");
        assert_eq!(app.pinned_row_count(), MAX_PINNED_PROCESSES);
        assert_eq!(app.process_action_message(), Some("Pin limit (8) reached"));
    }

    #[test]
    fn a_reused_pid_never_inherits_a_pin() {
        let mut app = processes_app(3);
        pin(&mut app, 2);

        let mut reused = process(2, "impostor");
        reused.start_time = 999;
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "p1"),
            reused,
            process(3, "p3"),
        ])));

        let pinned = app.process_row_at(0).unwrap();
        assert!(pinned.exited, "the pinned identity is gone");
        assert_eq!(pinned.process.name, "p2");
        let impostor = (1..app.process_count())
            .filter_map(|index| app.process_row_at(index))
            .find(|row| row.process.name == "impostor")
            .expect("the new process is listed");
        assert!(!impostor.pinned);
    }

    #[test]
    fn exited_pins_linger_then_drop_with_one_redraw_when_visible() {
        let mut app = processes_app(3);
        pin(&mut app, 2);
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "p1"),
            process(3, "p3"),
        ])));
        assert!(app.process_row_at(0).unwrap().exited);

        let start = Instant::now();
        assert!(!app.update(Action::Tick(start)), "linger starts");
        assert!(!app.update(Action::Tick(start + Duration::from_secs(4))));
        assert!(app.process_row_at(0).unwrap().exited);

        assert!(app.update(Action::Tick(start + EXITED_PIN_LINGER)));
        assert_eq!(app.pinned_row_count(), 0);
        assert_eq!(visible_pids(&app), vec![1, 3]);
        assert!(!app.update(Action::Tick(start + EXITED_PIN_LINGER * 2)));
    }

    #[test]
    fn exited_pins_dropped_on_another_tab_do_not_redraw_it() {
        let mut app = processes_app(3);
        pin(&mut app, 2);
        app.update(Action::SelectTab(Tab::Overview));
        app.update(Action::ProcessesUpdated(processes(vec![process(1, "p1")])));

        let start = Instant::now();
        app.update(Action::Tick(start));
        assert!(!app.update(Action::Tick(start + EXITED_PIN_LINGER)));
        app.update(Action::SelectTab(Tab::Processes));
        assert_eq!(visible_pids(&app), vec![1]);
    }

    #[test]
    fn a_failed_snapshot_does_not_mark_pins_exited() {
        let mut app = processes_app(3);
        pin(&mut app, 2);

        app.update(Action::ProcessesUpdated(ProcessSnapshot {
            processes: Vec::new(),
            error: Some("proc unavailable".into()),
        }));

        let row = app.process_row_at(0).unwrap();
        assert!(row.pinned && !row.exited);
    }

    #[test]
    fn exited_pinned_rows_refuse_details_and_signals_but_can_be_unpinned() {
        let mut app = processes_app(3);
        pin(&mut app, 2);
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "p1"),
            process(3, "p3"),
        ])));
        assert_eq!(app.selected_process_identity(), Some(identity(2)));

        assert!(!app.update(Action::OpenProcessDetails));
        assert!(!app.update(Action::RequestProcessSignal(ProcessSignal::Term)));
        assert!(!app.update(Action::RequestProcessSignal(ProcessSignal::Kill)));
        assert!(app.process_signal_confirmation().is_none());

        assert!(app.update(Action::TogglePin));
        assert_eq!(visible_pids(&app), vec![1, 3]);
    }

    #[test]
    fn an_open_detail_closes_when_its_pinned_process_exits() {
        let mut app = processes_app(2);
        pin(&mut app, 1);
        app.update(Action::OpenProcessDetails);
        assert!(app.process_detail_visible());

        app.update(Action::ProcessesUpdated(processes(vec![process(2, "p2")])));

        assert!(!app.process_detail_visible());
    }

    #[test]
    fn search_dims_non_matching_pins_and_never_falls_back_to_them() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "bash"),
            process(2, "sshd"),
            process(3, "ssh-agent"),
        ])));
        pin(&mut app, 1);

        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('s'));
        app.update(Action::AppendProcessSearch('s'));

        assert_eq!(visible_pids(&app), vec![1, 2, 3], "the pin stays visible");
        assert!(app.process_row_at(0).unwrap().dimmed);
        assert_eq!(app.listed_process_count(), 2);
        assert_eq!(
            app.selected_process().map(|p| p.pid),
            Some(2),
            "the selection left the dimmed pin for the first match"
        );

        // Arrows cross into the pinned section; Enter opens the chosen row.
        assert!(app.update(Action::ProcessPrevious));
        assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
        assert!(app.update(Action::OpenProcessDetails));
        assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
    }

    #[test]
    fn a_dimmed_pin_selected_on_purpose_stays_selected_across_refreshes() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        let snapshot = vec![process(1, "bash"), process(2, "sshd")];
        app.update(Action::ProcessesUpdated(processes(snapshot.clone())));
        pin(&mut app, 1);
        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('s'));
        app.update(Action::ProcessPrevious);
        assert_eq!(app.selected_process().map(|p| p.pid), Some(1));

        let mut refreshed = snapshot;
        refreshed[1].cpu_percent = Some(50.0);
        app.update(Action::ProcessesUpdated(processes(refreshed)));

        assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
    }

    #[test]
    fn a_search_without_matches_selects_nothing_rather_than_a_dimmed_pin() {
        let mut app = processes_app(3);
        pin(&mut app, 2);
        app.update(Action::SelectProcess(identity(3)));

        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('z'));

        assert_eq!(visible_pids(&app), vec![2]);
        assert_eq!(app.selected_process_identity(), None);
        assert!(!app.update(Action::OpenProcessDetails));
    }

    #[test]
    fn moving_pins_reorders_them_and_the_selection_follows() {
        let mut app = processes_app(4);
        pin(&mut app, 1);
        pin(&mut app, 2);
        pin(&mut app, 3);
        assert_eq!(visible_pids(&app), vec![1, 2, 3, 4]);

        assert!(app.update(Action::MoveSelectedPin(PinMove::Up)));
        assert_eq!(visible_pids(&app), vec![1, 3, 2, 4]);
        assert!(app.update(Action::MoveSelectedPin(PinMove::Up)));
        assert_eq!(visible_pids(&app), vec![3, 1, 2, 4]);
        assert_eq!(app.selected_process_index(), Some(0));
        assert!(!app.update(Action::MoveSelectedPin(PinMove::Up)), "top");

        app.update(Action::SelectProcess(identity(2)));
        assert!(
            !app.update(Action::MoveSelectedPin(PinMove::Down)),
            "bottom"
        );
        app.update(Action::SelectProcess(identity(4)));
        assert!(
            !app.update(Action::MoveSelectedPin(PinMove::Up)),
            "unpinned rows do not move"
        );
    }

    #[test]
    fn pin_actions_need_the_processes_tab_and_no_overlay() {
        let mut app = processes_app(3);
        pin(&mut app, 1);
        pin(&mut app, 2);

        app.update(Action::SelectTab(Tab::Overview));
        assert!(!app.update(Action::TogglePin));
        assert!(!app.update(Action::MoveSelectedPin(PinMove::Up)));

        app.update(Action::SelectTab(Tab::Processes));
        for open in [
            Action::ShowHelp,
            Action::OpenProcessDetails,
            Action::RequestProcessSignal(ProcessSignal::Term),
        ] {
            assert!(app.update(open.clone()), "{open:?}");
            assert!(!app.update(Action::TogglePin), "{open:?}");
            assert!(
                !app.update(Action::MoveSelectedPin(PinMove::Up)),
                "{open:?}"
            );
            app.update(Action::Escape);
        }
        assert_eq!(visible_pids(&app), vec![1, 2, 3]);
    }

    #[test]
    fn a_pending_signal_keeps_its_target_while_rows_change() {
        let temp_dir =
            std::env::temp_dir().join(format!("tuxctl_pin_signal_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(temp_dir.join("2")).unwrap();
        std::fs::write(
            temp_dir.join("2/stat"),
            "2 (p2) S 1 2 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 2 0 0 0 0 0 0 0 0 0 0 0 0",
        )
        .unwrap();

        let mut app = processes_app(3);
        pin(&mut app, 3);
        pin(&mut app, 2);
        assert!(app.update(Action::RequestProcessSignal(ProcessSignal::Term)));

        // A refresh reorders everything and a pin move is attempted.
        let mut refreshed = cpu_ranked(4);
        refreshed.reverse();
        app.update(Action::ProcessesUpdated(processes(refreshed)));
        assert!(!app.update(Action::MoveSelectedPin(PinMove::Up)));
        assert!(!app.update(Action::SelectProcess(identity(1))));

        let mut sent_to = None;
        app.confirm_process_signal_at(&temp_dir, Ok, |pid, _| {
            sent_to = Some(*pid);
            Ok(())
        });
        let _ = std::fs::remove_dir_all(&temp_dir);
        assert_eq!(sent_to, Some(2));
        assert_eq!(app.process_action_message(), Some("Sent SIGTERM to p2 (2)"));
    }

    #[test]
    fn process_search_clears_action_message() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.process_action_message = Some("Previous message".to_string());
        assert_eq!(app.process_action_message(), Some("Previous message"));

        app.update(Action::BeginProcessSearch);
        assert!(app.process_action_message().is_none());
    }
}
