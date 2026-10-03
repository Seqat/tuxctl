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

/// Which processes the table lists, cycled with `v`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessView {
    All,
    HideKernelThreads,
}

impl ProcessView {
    fn next(self) -> Self {
        match self {
            Self::All => Self::HideKernelThreads,
            Self::HideKernelThreads => Self::All,
        }
    }

    fn allows(self, process: &ProcessInfo) -> bool {
        self != Self::HideKernelThreads || !process.kernel_thread
    }
}

/// One row of the Processes table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessRow {
    /// Index into `processes`. `dimmed` marks a pinned process that does not
    /// match the search or view filter; it stays listed but is never a
    /// fallback selection.
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

    /// Status text of an active view filter.
    pub fn process_view_label(&self) -> Option<&'static str> {
        (self.process_view == ProcessView::HideKernelThreads).then_some("no kernel threads")
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
        // Overview lists pinned processes, so their values changing is visible there.
        let overview_pins_changed =
            overview_visible && self.pinned_values_changed(&snapshot.processes);

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
            return overview_summary_changed
                || overview_pins_changed
                || (overview_visible && was_stale);
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
        let view = self.process_view;
        // One pass: live pinned processes go to their slot (at most
        // MAX_PINNED_PROCESSES identity comparisons each), the rest are filtered.
        let mut pinned_rows = [None; MAX_PINNED_PROCESSES];
        let mut rows = std::mem::take(&mut self.filtered_processes);
        rows.clear();
        for (index, (process, keys)) in processes.iter().zip(keys).enumerate() {
            let matches = view.allows(process) && process_matches(process, keys, &query);
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
        rows.sort_by(|left, right| match (*left, *right) {
            (ProcessRow::Live { index: left, .. }, ProcessRow::Live { index: right, .. }) => {
                compare_processes(
                    (&processes[left], &keys[left]),
                    (&processes[right], &keys[right]),
                    sort,
                )
            }
            // Only live rows are sorted here; exited pins join below.
            _ => std::cmp::Ordering::Equal,
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

    pub(super) fn cycle_process_view(&mut self) -> bool {
        self.process_view = self.process_view.next();
        self.rebuild_process_filter();
        self.leave_dimmed_selection();
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
        matches!(self.active_tab, Tab::Processes | Tab::Overview)
    }

    /// Pinned processes in the user's order, independent of the Processes
    /// table (which is not rebuilt while that tab is hidden).
    pub fn pinned_processes(&self) -> impl Iterator<Item = ProcessRowView<'_>> + '_ {
        self.pinned.iter().filter_map(|pin| {
            let (process, exited) = match &pin.exited {
                Some(exited) => (&exited.last_seen, true),
                None => (
                    self.processes
                        .iter()
                        .find(|process| process.identity() == pin.identity)?,
                    false,
                ),
            };
            Some(ProcessRowView {
                process,
                pinned: true,
                dimmed: false,
                exited,
            })
        })
    }

    /// Whether a running pinned process shows different CPU or memory values
    /// in `next`, or is missing from it (it exited).
    fn pinned_values_changed(&self, next: &[ProcessInfo]) -> bool {
        self.pinned
            .iter()
            .filter(|pin| pin.exited.is_none())
            .any(|pin| {
                let shown = |processes: &[ProcessInfo]| {
                    processes
                        .iter()
                        .find(|process| process.identity() == pin.identity)
                        .map(|process| (process.cpu_percent, process.memory_bytes))
                };
                shown(&self.processes) != shown(next)
            })
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
mod tests;
