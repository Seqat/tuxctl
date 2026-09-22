//! Processes screen: snapshot handling, search, sorting, selection and signal confirmation.

use super::*;

impl App {
    pub fn process_count(&self) -> usize {
        self.filtered_processes.len()
    }

    pub fn process_summary(&self) -> ProcessSummary {
        self.process_summary
    }

    pub fn process_at(&self, index: usize) -> Option<&ProcessInfo> {
        self.filtered_processes
            .get(index)
            .and_then(|index| self.processes.get(*index))
    }

    pub fn selected_process_index(&self) -> Option<usize> {
        let selected = self.selected_process?;
        self.filtered_processes.iter().position(|index| {
            self.processes
                .get(*index)
                .is_some_and(|process| process.identity() == selected)
        })
    }

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
            self.processes = snapshot.processes;
            self.process_keys.clear();
            self.process_summary = summary;
            self.process_error = None;
            self.close_confirmation_for_exited_process();
            return overview_summary_changed || (overview_visible && was_stale);
        }

        let previous_index = self.selected_process_index().unwrap_or(0);
        let previous_selection = self.selected_process;
        self.processes = snapshot.processes;
        self.process_keys.clear();
        self.process_summary = summary;
        self.process_error = None;
        self.rebuild_process_filter();

        self.selected_process = previous_selection.filter(|identity| {
            self.filtered_processes.iter().any(|index| {
                self.processes
                    .get(*index)
                    .is_some_and(|process| process.identity() == *identity)
            })
        });

        self.close_confirmation_for_exited_process();

        if self.selected_process.is_none() {
            let replacement = previous_index.min(self.filtered_processes.len().saturating_sub(1));
            self.selected_process = self.process_at(replacement).map(ProcessInfo::identity);
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
            true
        } else {
            false
        }
    }

    pub(super) fn append_process_search(&mut self, character: char) -> bool {
        if self.process_searching && !character.is_control() {
            self.process_search_query.push(character);
            self.rebuild_process_filter();
            true
        } else {
            false
        }
    }

    pub(super) fn backspace_process_search(&mut self) -> bool {
        if self.process_searching && self.process_search_query.pop().is_some() {
            self.rebuild_process_filter();
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
        self.filtered_processes = self
            .processes
            .iter()
            .zip(keys)
            .enumerate()
            .filter(|(_, (process, keys))| process_matches(process, keys, &query))
            .map(|(index, _)| index)
            .collect();
        let processes = &self.processes;
        let sort = self.process_sort;
        self.filtered_processes.sort_by(|&left, &right| {
            compare_processes(
                (&processes[left], &keys[left]),
                (&processes[right], &keys[right]),
                sort,
            )
        });

        self.selected_process = previous_selection.filter(|identity| {
            self.filtered_processes.iter().any(|index| {
                self.processes
                    .get(*index)
                    .is_some_and(|process| process.identity() == *identity)
            })
        });
        if self.selected_process.is_none() {
            let replacement = previous_index.min(self.filtered_processes.len().saturating_sub(1));
            self.selected_process = self.process_at(replacement).map(ProcessInfo::identity);
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

        if let Some(index) = self.filtered_processes.iter().position(|index| {
            self.processes
                .get(*index)
                .is_some_and(|process| process.identity() == identity)
        }) {
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
            let remains_visible = self.filtered_processes.iter().any(|index| {
                self.processes
                    .get(*index)
                    .is_some_and(|process| process.identity() == *identity)
            });
            if !remains_visible {
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
