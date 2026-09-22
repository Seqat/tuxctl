//! Central application state: the `App` struct, action dispatch and tab/overlay handling.
//! Per-screen state transitions live in the sibling modules.

use std::{
    cmp::Ordering,
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::{
    action::{
        Action, InputMode, MouseTarget, ProcessSort, ProcessSortField, SignalConfirmButton, Tab,
    },
    linux::{
        send_process_signal, HardwareInventory, JournalBatch, JournalEntry, NetworkInterfaceInfo,
        NetworkSnapshot, ProcessIdentity, ProcessInfo, ProcessSignal, ProcessSignalError,
        ProcessSnapshot, ProcessSummary, ServiceInfo, ServiceRefreshGeneration, ServiceSnapshot,
        SystemMetrics, SYSTEMCTL_TIMEOUT,
    },
};

#[cfg(test)]
use crate::linux::verify_and_send_signal_at;

mod health;
mod logs;
mod network;
mod processes;
mod services;
#[cfg(test)]
mod test_support;

pub use health::Collector;
use health::CollectorHealth;
use processes::ProcessKeys;

const LOG_BUFFER_CAPACITY: usize = 2_000;
const AGGREGATE_CPU_HISTORY_CAPACITY: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSignalConfirmation {
    pub identity: ProcessIdentity,
    pub name: String,
    pub signal: ProcessSignal,
    pub focused_button: SignalConfirmButton,
}

#[derive(Debug)]
pub struct AggregateCpuHistory {
    samples: VecDeque<f64>,
}

impl Default for AggregateCpuHistory {
    fn default() -> Self {
        Self {
            samples: VecDeque::with_capacity(AGGREGATE_CPU_HISTORY_CAPACITY),
        }
    }
}

impl AggregateCpuHistory {
    fn push(&mut self, utilization_percent: f64) -> bool {
        if !utilization_percent.is_finite() {
            return false;
        }
        if self.samples.len() == AGGREGATE_CPU_HISTORY_CAPACITY {
            self.samples.pop_front();
        }
        self.samples
            .push_back(utilization_percent.clamp(0.0, 100.0));
        true
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = f64> + ExactSizeIterator + '_ {
        self.samples.iter().copied()
    }
}

/// The modal layer shown above the active screen; at most one is open at a time.
///
/// v0.3.0 adds the Esc main menu, About and Options pages here.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    Help,
    ProcessDetail,
    ServiceDetail,
    LogDetail,
    NetworkDetail,
    ProcessSignal(ProcessSignalConfirmation),
}

#[derive(Debug)]
pub struct App {
    overlay: Option<Overlay>,
    should_quit: bool,
    active_tab: Tab,
    system_metrics: SystemMetrics,
    hardware: Option<HardwareInventory>,
    aggregate_cpu_history: AggregateCpuHistory,
    processes: Vec<ProcessInfo>,
    /// Lowercased search/sort keys parallel to `processes`; empty until the next
    /// filter rebuild after a snapshot replaced `processes`.
    process_keys: Vec<ProcessKeys>,
    process_summary: ProcessSummary,
    filtered_processes: Vec<usize>,
    /// Set while Processes is hidden and `filtered_processes` has been cleared
    /// instead of rebuilt; holds the selected row index to fall back to.
    deferred_process_rebuild: Option<usize>,
    selected_process: Option<ProcessIdentity>,
    process_scroll: usize,
    process_view_height: usize,
    process_search_query: String,
    process_searching: bool,
    process_error: Option<String>,
    process_sort: ProcessSort,
    process_action_message: Option<String>,
    services: Vec<ServiceInfo>,
    filtered_services: Vec<usize>,
    selected_service: Option<String>,
    service_scroll: usize,
    service_view_height: usize,
    service_search_query: String,
    service_searching: bool,
    service_error: Option<String>,
    service_refresh_generation: ServiceRefreshGeneration,
    service_refresh_requested: Option<ServiceRefreshGeneration>,
    pending_service_refresh_generation: Option<ServiceRefreshGeneration>,
    logs: VecDeque<JournalEntry>,
    filtered_logs: Vec<usize>,
    selected_log: Option<u64>,
    log_scroll: usize,
    log_view_height: usize,
    log_search_query: String,
    log_searching: bool,
    log_following: bool,
    log_paused: bool,
    log_dropped: usize,
    log_error: Option<String>,
    networks: Vec<NetworkInterfaceInfo>,
    selected_network: Option<String>,
    network_scroll: usize,
    network_view_height: usize,
    network_error: Option<String>,
    hovered: Option<MouseTarget>,
    collector_health: [CollectorHealth; 4],
    logs_visited: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            overlay: None,
            should_quit: false,
            active_tab: Tab::Overview,
            system_metrics: SystemMetrics::default(),
            hardware: None,
            aggregate_cpu_history: AggregateCpuHistory::default(),
            processes: Vec::new(),
            process_keys: Vec::new(),
            process_summary: ProcessSummary::default(),
            filtered_processes: Vec::new(),
            deferred_process_rebuild: None,
            selected_process: None,
            process_scroll: 0,
            process_view_height: 0,
            process_search_query: String::new(),
            process_searching: false,
            process_error: None,
            process_sort: ProcessSort::default(),
            process_action_message: None,
            services: Vec::new(),
            filtered_services: Vec::new(),
            selected_service: None,
            service_scroll: 0,
            service_view_height: 0,
            service_search_query: String::new(),
            service_searching: false,
            service_error: None,
            service_refresh_generation: 0,
            service_refresh_requested: None,
            pending_service_refresh_generation: None,
            logs: VecDeque::with_capacity(LOG_BUFFER_CAPACITY),
            filtered_logs: Vec::new(),
            selected_log: None,
            log_scroll: 0,
            log_view_height: 0,
            log_search_query: String::new(),
            log_searching: false,
            log_following: true,
            log_paused: false,
            log_dropped: 0,
            log_error: None,
            networks: Vec::new(),
            selected_network: None,
            network_scroll: 0,
            network_view_height: 0,
            network_error: None,
            hovered: None,
            collector_health: [CollectorHealth::default(); 4],
            logs_visited: false,
        }
    }
}

impl App {
    /// Services are only collected while their tab is visible.
    pub fn services_visible(&self) -> bool {
        self.active_tab == Tab::Services
    }

    /// Whether the journal stream is needed yet; it starts on the first Logs visit.
    pub fn logs_visited(&self) -> bool {
        self.logs_visited
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn active_tab(&self) -> Tab {
        self.active_tab
    }

    pub fn help_visible(&self) -> bool {
        self.overlay == Some(Overlay::Help)
    }

    pub fn system_metrics(&self) -> &SystemMetrics {
        &self.system_metrics
    }

    pub fn aggregate_cpu_history(&self) -> &AggregateCpuHistory {
        &self.aggregate_cpu_history
    }

    pub fn hardware(&self) -> Option<&HardwareInventory> {
        self.hardware.as_ref()
    }

    pub fn input_mode(&self) -> InputMode {
        if let Some(overlay) = &self.overlay {
            match overlay {
                Overlay::Help => InputMode::Help,
                Overlay::ProcessSignal(_) => InputMode::ProcessSignalConfirm,
                Overlay::ProcessDetail => InputMode::ProcessDetail,
                Overlay::ServiceDetail => InputMode::ServiceDetail,
                Overlay::LogDetail => InputMode::LogDetail,
                Overlay::NetworkDetail => InputMode::NetworkDetail,
            }
        } else if self.process_searching {
            InputMode::ProcessSearch
        } else if self.service_searching {
            InputMode::ServiceSearch
        } else if self.log_searching {
            InputMode::LogSearch
        } else if self.active_tab == Tab::Services {
            InputMode::Services
        } else if self.active_tab == Tab::Logs {
            InputMode::Logs
        } else if self.active_tab == Tab::Network {
            InputMode::Network
        } else {
            InputMode::Normal
        }
    }

    pub fn hovered(&self) -> Option<&MouseTarget> {
        self.hovered.as_ref()
    }

    /// Applies an action and reports whether the rendered UI may have changed.
    pub fn update(&mut self, action: Action) -> bool {
        // Terminal geometry is global state and must bypass every input/modal guard below.
        if matches!(&action, Action::Resize) {
            self.hovered = None;
            return true;
        }

        match action {
            Action::Quit => {
                self.should_quit = true;
                false
            }
            Action::SystemMetricsUpdated(metrics) => {
                self.record_collector_update(Collector::Metrics);
                let process_metrics_changed = self.system_metrics.cpu_percent
                    != metrics.cpu_percent
                    || self.system_metrics.memory != metrics.memory;
                let history_changed = metrics
                    .cpu_percent
                    .is_some_and(|sample| self.aggregate_cpu_history.push(sample));
                let metrics_changed = self.system_metrics != metrics;
                if metrics_changed {
                    self.system_metrics = metrics;
                }
                match self.active_tab {
                    Tab::Overview => metrics_changed || history_changed,
                    Tab::Processes => process_metrics_changed,
                    _ => false,
                }
            }
            Action::HardwareDiscovered(hardware) => {
                if self.hardware.as_ref() == Some(&hardware) {
                    false
                } else {
                    self.hardware = Some(hardware);
                    self.active_tab == Tab::Overview
                }
            }
            Action::ProcessesUpdated(snapshot) => {
                self.record_collector_update(Collector::Processes);
                self.update_processes(snapshot)
            }
            Action::ServicesUpdated(snapshot) => {
                self.record_collector_update(Collector::Services);
                self.update_services(snapshot)
            }
            Action::LogsUpdated(batch) => self.update_logs(batch),
            Action::NetworkUpdated(snapshot) => {
                self.record_collector_update(Collector::Network);
                self.update_networks(snapshot)
            }
            Action::ProcessViewportChanged { start, height } => {
                self.process_view_height = height;
                self.process_scroll = start;
                self.ensure_process_visible();
                false
            }
            Action::ServiceViewportChanged { start, height } => {
                self.service_view_height = height;
                self.service_scroll = start;
                self.ensure_service_visible();
                false
            }
            Action::LogViewportChanged { start, height } => {
                self.log_view_height = height;
                self.log_scroll = start;
                self.ensure_log_visible();
                false
            }
            Action::NetworkViewportChanged { start, height } => {
                self.network_view_height = height;
                self.network_scroll = start;
                self.ensure_network_visible();
                false
            }
            Action::HoverMouseTarget(target) => {
                if self.hovered == target {
                    false
                } else {
                    self.hovered = target;
                    true
                }
            }
            // Staleness is global state, so it is checked even while a modal is open.
            Action::Tick(now) => self.check_collector_staleness(now),
            Action::Escape => self.escape(),
            Action::CancelProcessSignal => self.cancel_process_signal(),
            Action::ConfirmProcessSignal => self.confirm_process_signal(),
            Action::ToggleProcessSignalFocus => self.toggle_process_signal_focus(),
            Action::FocusProcessSignal(button) => self.focus_process_signal(button),
            Action::ExecuteFocusedProcessSignal => self.execute_focused_process_signal(),
            Action::Resize => unreachable!("resize actions return before modal suppression"),
            _ if self.overlay.is_some() => false,
            Action::ShowHelp => {
                self.overlay = Some(Overlay::Help);
                self.hovered = None;
                true
            }
            Action::SelectTab(tab) => self.select_tab(tab),
            Action::NextTab => self.select_tab(self.active_tab.next()),
            Action::PreviousTab => self.select_tab(self.active_tab.previous()),
            Action::ProcessPrevious => self.move_process_selection(-1),
            Action::ProcessNext => self.move_process_selection(1),
            Action::ProcessPreviousPage => {
                self.move_process_selection(-(self.process_view_height.max(1) as isize))
            }
            Action::ProcessNextPage => {
                self.move_process_selection(self.process_view_height.max(1) as isize)
            }
            Action::ProcessFirst => self.select_process_index(0),
            Action::ProcessLast => {
                self.select_process_index(self.filtered_processes.len().saturating_sub(1))
            }
            Action::SelectProcess(identity) => self.select_process(identity),
            Action::BeginProcessSearch => self.begin_process_search(),
            Action::AppendProcessSearch(character) => self.append_process_search(character),
            Action::BackspaceProcessSearch => self.backspace_process_search(),
            Action::OpenProcessDetails => self.open_process_details(),
            Action::RequestProcessSignal(signal) => self.request_process_signal(signal),
            Action::SortProcesses(field) => self.sort_processes(field),
            Action::ServicePrevious => self.move_service_selection(-1),
            Action::ServiceNext => self.move_service_selection(1),
            Action::ServicePreviousPage => {
                self.move_service_selection(-(self.service_view_height.max(1) as isize))
            }
            Action::ServiceNextPage => {
                self.move_service_selection(self.service_view_height.max(1) as isize)
            }
            Action::ServiceFirst => self.select_service_index(0),
            Action::ServiceLast => {
                self.select_service_index(self.filtered_services.len().saturating_sub(1))
            }
            Action::SelectService(unit) => self.select_service(&unit),
            Action::BeginServiceSearch => self.begin_service_search(),
            Action::AppendServiceSearch(character) => self.append_service_search(character),
            Action::BackspaceServiceSearch => self.backspace_service_search(),
            Action::OpenServiceDetails => self.open_service_details(),
            Action::RefreshServices => self.request_service_refresh(),
            Action::LogPrevious => self.move_log_selection(-1),
            Action::LogNext => self.move_log_selection(1),
            Action::LogPreviousPage => {
                self.move_log_selection(-(self.log_view_height.max(1) as isize))
            }
            Action::LogNextPage => self.move_log_selection(self.log_view_height.max(1) as isize),
            Action::LogFirst => self.select_log_index(0),
            Action::LogLast => self.select_log_index(self.filtered_logs.len().saturating_sub(1)),
            Action::SelectLog(id) => self.select_log(id),
            Action::BeginLogSearch => self.begin_log_search(),
            Action::AppendLogSearch(character) => self.append_log_search(character),
            Action::BackspaceLogSearch => self.backspace_log_search(),
            Action::OpenLogDetails => self.open_log_details(),
            Action::ToggleLogFollow => self.toggle_log_follow(),
            Action::ToggleLogPause => self.toggle_log_pause(),
            Action::NetworkPrevious => self.move_network_selection(-1),
            Action::NetworkNext => self.move_network_selection(1),
            Action::NetworkPreviousPage => {
                self.move_network_selection(-(self.network_view_height.max(1) as isize))
            }
            Action::NetworkNextPage => {
                self.move_network_selection(self.network_view_height.max(1) as isize)
            }
            Action::NetworkFirst => self.select_network_index(0),
            Action::NetworkLast => self.select_network_index(self.networks.len().saturating_sub(1)),
            Action::SelectNetwork(name) => self.select_network(&name),
            Action::OpenNetworkDetails => self.open_network_details(),
        }
    }

    /// Closes `overlay` if it is the one open; other overlays stay.
    fn close_overlay(&mut self, overlay: &Overlay) {
        if self.overlay.as_ref() == Some(overlay) {
            self.overlay = None;
        }
    }

    fn select_tab(&mut self, tab: Tab) -> bool {
        let entering_services = tab == Tab::Services && self.active_tab != Tab::Services;
        // Tab actions are blocked while an overlay is open, so none is open here.
        let changed = self.active_tab != tab
            || self.process_searching
            || self.service_searching
            || self.log_searching
            || self.hovered.is_some();
        self.active_tab = tab;
        self.process_action_message = None;
        self.process_searching = false;
        self.service_searching = false;
        self.log_searching = false;
        self.hovered = None;
        if tab == Tab::Processes && self.deferred_process_rebuild.is_some() {
            self.rebuild_process_filter();
        }
        if tab == Tab::Logs {
            self.logs_visited = true;
        }
        if entering_services {
            // The collector was paused while hidden; fetch current state now.
            self.request_service_refresh();
        }
        changed
    }

    fn escape(&mut self) -> bool {
        if matches!(self.overlay, Some(Overlay::ProcessSignal(_))) {
            self.cancel_process_signal()
        } else if self.overlay.take().is_some() {
            true
        } else {
            match self.active_tab {
                Tab::Processes
                    if self.process_searching || !self.process_search_query.is_empty() =>
                {
                    self.process_searching = false;
                    self.process_search_query.clear();
                    self.rebuild_process_filter();
                    true
                }
                Tab::Services
                    if self.service_searching || !self.service_search_query.is_empty() =>
                {
                    self.service_searching = false;
                    self.service_search_query.clear();
                    self.rebuild_service_filter();
                    true
                }
                Tab::Logs if self.log_searching || !self.log_search_query.is_empty() => {
                    self.log_searching = false;
                    self.log_search_query.clear();
                    self.rebuild_log_filter();
                    true
                }
                _ => false,
            }
        }
    }
}

pub(crate) fn calculate_scroll(
    item_count: usize,
    selected: Option<usize>,
    requested_start: usize,
    viewport_height: usize,
) -> usize {
    if item_count == 0 {
        return 0;
    }

    if viewport_height == 0 {
        return selected.unwrap_or(requested_start).min(item_count - 1);
    }

    let max_start = item_count.saturating_sub(viewport_height);
    let mut start = requested_start.min(max_start);
    if let Some(selected) = selected.map(|selected| selected.min(item_count - 1)) {
        if selected < start {
            start = selected;
        } else if selected >= start.saturating_add(viewport_height) {
            start = selected.saturating_add(1).saturating_sub(viewport_height);
        }
    }

    start.min(max_start)
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[derive(Debug, Clone, Copy)]
    enum OverlayKind {
        Help,
        ProcessDetail,
        ProcessSignal,
        ServiceDetail,
        LogDetail,
        NetworkDetail,
    }

    const OVERLAYS: [OverlayKind; 6] = [
        OverlayKind::Help,
        OverlayKind::ProcessDetail,
        OverlayKind::ProcessSignal,
        OverlayKind::ServiceDetail,
        OverlayKind::LogDetail,
        OverlayKind::NetworkDetail,
    ];

    /// An app with data on every screen and `kind` open over its screen.
    fn app_with_overlay(kind: OverlayKind) -> App {
        let mut app = App::default();
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "init"),
            process(2, "worker"),
        ])));
        app.update(Action::ServicesUpdated(services(vec![service(
            "sshd.service",
            "active",
            "OpenSSH",
        )])));
        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            1, "kernel", 6, "boot",
        )])));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0")],
            error: None,
        }));
        let (tab, open) = match kind {
            OverlayKind::Help => (Tab::Overview, Action::ShowHelp),
            OverlayKind::ProcessDetail => (Tab::Processes, Action::OpenProcessDetails),
            OverlayKind::ProcessSignal => (
                Tab::Processes,
                Action::RequestProcessSignal(ProcessSignal::Term),
            ),
            OverlayKind::ServiceDetail => (Tab::Services, Action::OpenServiceDetails),
            OverlayKind::LogDetail => (Tab::Logs, Action::OpenLogDetails),
            OverlayKind::NetworkDetail => (Tab::Network, Action::OpenNetworkDetails),
        };
        app.update(Action::SelectTab(tab));
        assert!(app.update(open), "{kind:?} did not open");
        assert!(app.overlay.is_some());
        app
    }

    #[test]
    fn open_overlay_blocks_tab_and_navigation_actions() {
        for kind in OVERLAYS {
            let mut app = app_with_overlay(kind);
            let tab = app.active_tab();
            let mode = app.input_mode();

            for action in [
                Action::SelectTab(Tab::Logs),
                Action::NextTab,
                Action::PreviousTab,
                Action::ShowHelp,
                Action::ProcessNext,
                Action::ServiceNext,
                Action::LogNext,
                Action::NetworkNext,
                Action::BeginProcessSearch,
                Action::OpenProcessDetails,
                Action::RequestProcessSignal(ProcessSignal::Kill),
            ] {
                assert!(
                    !app.update(action.clone()),
                    "{kind:?} let {action:?} through"
                );
            }
            assert_eq!(app.active_tab(), tab, "{kind:?}");
            assert_eq!(app.input_mode(), mode, "{kind:?}");
        }
    }

    #[test]
    fn escape_closes_exactly_one_overlay_per_press() {
        for kind in OVERLAYS {
            let mut app = app_with_overlay(kind);

            assert!(app.update(Action::Escape), "{kind:?}");
            assert!(app.overlay.is_none(), "{kind:?}");
            assert!(
                !app.update(Action::Escape),
                "{kind:?}: nothing left to close"
            );
        }
    }

    #[test]
    fn escape_closes_a_detail_before_clearing_the_search_behind_it() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(
            1, "init",
        )])));
        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('i'));
        app.update(Action::OpenProcessDetails);
        assert!(app.process_detail_visible());

        assert!(app.update(Action::Escape));
        assert!(!app.process_detail_visible());
        assert_eq!(
            app.process_search_query(),
            "i",
            "the first Esc only closes the detail"
        );

        assert!(app.update(Action::Escape));
        assert_eq!(app.process_search_query(), "");
    }

    #[test]
    fn background_snapshots_still_apply_while_an_overlay_is_open() {
        for kind in OVERLAYS {
            let mut app = app_with_overlay(kind);

            app.update(Action::SystemMetricsUpdated(SystemMetrics {
                cpu_percent: Some(42.0),
                ..SystemMetrics::default()
            }));
            app.update(Action::ProcessesUpdated(processes(vec![
                process(1, "init"),
                process(2, "worker"),
                process(3, "new"),
            ])));

            assert_eq!(app.system_metrics().cpu_percent, Some(42.0), "{kind:?}");
            assert_eq!(app.process_summary().total, 3, "{kind:?}");
            assert!(
                app.overlay.is_some(),
                "{kind:?} closed on a background update"
            );
        }
    }

    #[test]
    fn signal_actions_leave_other_overlays_open() {
        for kind in [OverlayKind::Help, OverlayKind::ProcessDetail] {
            let mut app = app_with_overlay(kind);

            for action in [
                Action::ConfirmProcessSignal,
                Action::CancelProcessSignal,
                Action::ExecuteFocusedProcessSignal,
                Action::ToggleProcessSignalFocus,
            ] {
                assert!(
                    !app.update(action.clone()),
                    "{kind:?} reacted to {action:?}"
                );
                assert!(app.overlay.is_some(), "{action:?} closed {kind:?}");
            }
        }
    }

    #[test]
    fn quit_action_stops_the_app() {
        let mut app = App::default();

        app.update(Action::Quit);

        assert!(app.should_quit());
    }

    #[test]
    fn tab_navigation_wraps_in_both_directions() {
        let mut app = App::default();

        app.update(Action::PreviousTab);
        assert_eq!(app.active_tab(), Tab::Network);

        app.update(Action::NextTab);
        assert_eq!(app.active_tab(), Tab::Overview);
    }

    #[test]
    fn selecting_a_tab_updates_central_state() {
        let mut app = App::default();

        app.update(Action::SelectTab(Tab::Services));

        assert_eq!(app.active_tab(), Tab::Services);
    }

    #[test]
    fn help_is_modal_until_closed() {
        let mut app = App::default();

        app.update(Action::ShowHelp);
        app.update(Action::NextTab);
        assert!(app.help_visible());
        assert_eq!(app.active_tab(), Tab::Overview);

        app.update(Action::Escape);
        assert!(!app.help_visible());
    }

    #[test]
    fn journal_is_needed_only_after_logs_is_visited() {
        let mut app = App::default();
        for tab in [Tab::Processes, Tab::Services, Tab::Network, Tab::Overview] {
            app.update(Action::SelectTab(tab));
            assert!(!app.logs_visited(), "{tab:?}");
        }

        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::SelectTab(Tab::Overview));

        assert!(
            app.logs_visited(),
            "the journal keeps streaming once started"
        );
    }

    #[test]
    fn hover_only_invalidates_rendering_on_semantic_transitions() {
        let mut app = App::default();
        let target = MouseTarget::Tab(Tab::Processes);

        assert!(app.update(Action::HoverMouseTarget(Some(target.clone()))));
        assert!(!app.update(Action::HoverMouseTarget(Some(target))));
        assert!(app.update(Action::HoverMouseTarget(None)));
        assert!(!app.update(Action::HoverMouseTarget(None)));
    }

    #[test]
    fn scroll_calculation_handles_resize_and_empty_viewports() {
        assert_eq!(calculate_scroll(20, Some(10), 0, 5), 6);
        assert_eq!(calculate_scroll(20, Some(10), 6, 2), 9);
        assert_eq!(calculate_scroll(3, Some(2), usize::MAX, 20), 0);
        assert_eq!(calculate_scroll(20, Some(10), 0, 0), 10);
        assert_eq!(calculate_scroll(0, None, usize::MAX, 0), 0);
    }

    #[test]
    fn inactive_screen_snapshot_updates_do_not_trigger_redraw() {
        let mut app = App::default();
        assert_eq!(app.active_tab(), Tab::Overview);

        // Process summary changes affect the active Overview and require a redraw.
        let proc_redraw = app.update(Action::ProcessesUpdated(processes(vec![process(
            1, "test",
        )])));
        assert!(proc_redraw);
        assert_eq!(app.process_summary().total, 1);

        // Process details may refresh without changing the summary shown on Overview.
        let same_summary_redraw = app.update(Action::ProcessesUpdated(processes(vec![process(
            2,
            "replacement",
        )])));
        assert!(!same_summary_redraw);

        // Services update while on Overview tab should return false, but update state
        let srv_redraw = app.update(Action::ServicesUpdated(services(vec![service(
            "test.service",
            "active",
            "running",
        )])));
        assert!(!srv_redraw);
        assert_eq!(app.service_count(), 1);

        // Network updates affect the summary shown on the active Overview.
        let net_redraw = app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0")],
            error: None,
        }));
        assert!(net_redraw);
        assert_eq!(app.network_count(), 1);

        // Overview update while on Overview tab DOES trigger redraw
        let metrics = SystemMetrics {
            cpu_percent: Some(42.0),
            ..Default::default()
        };
        let ov_redraw = app.update(Action::SystemMetricsUpdated(metrics.clone()));
        assert!(ov_redraw);

        // Switching to Processes tab triggers redraw
        assert!(app.update(Action::SelectTab(Tab::Processes)));

        // Visible system metrics update the active Processes summary.
        let metrics2 = SystemMetrics {
            cpu_percent: Some(99.0),
            memory: Some(crate::linux::ByteUsage { used: 1, total: 4 }),
            ..Default::default()
        };
        let process_metrics_redraw = app.update(Action::SystemMetricsUpdated(metrics2.clone()));
        assert!(process_metrics_redraw);

        let mut memory_only_metrics = metrics2;
        memory_only_metrics.memory = Some(crate::linux::ByteUsage { used: 2, total: 4 });
        assert!(app.update(Action::SystemMetricsUpdated(memory_only_metrics.clone())));

        // Unrelated Overview-only fields do not redraw Processes.
        let mut overview_only_metrics = memory_only_metrics;
        overview_only_metrics.uptime = Some(std::time::Duration::from_secs(60));
        assert!(!app.update(Action::SystemMetricsUpdated(overview_only_metrics)));

        let net_redraw_inactive = app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("wlan0")],
            error: None,
        }));
        assert!(!net_redraw_inactive);

        // Process update while on Processes tab DOES trigger redraw
        let proc_redraw_active =
            app.update(Action::ProcessesUpdated(processes(vec![process(2, "new")])));
        assert!(proc_redraw_active);

        // System metrics remain cached without redrawing unrelated active tabs.
        assert!(app.update(Action::SelectTab(Tab::Services)));
        assert!(!app.update(Action::SystemMetricsUpdated(SystemMetrics {
            cpu_percent: Some(12.0),
            memory: Some(crate::linux::ByteUsage { used: 1, total: 4 }),
            ..Default::default()
        })));
    }

    #[test]
    fn overview_collector_health_transitions_redraw_only_while_visible() {
        let mut app = App::default();
        let process_snapshot = processes(vec![process(1, "init")]);
        let network_snapshot = NetworkSnapshot {
            interfaces: vec![dummy_network("eth0")],
            error: None,
        };
        app.update(Action::ProcessesUpdated(process_snapshot.clone()));
        app.update(Action::NetworkUpdated(network_snapshot.clone()));
        let summary = app.process_summary();

        assert!(app.update(Action::ProcessesUpdated(ProcessSnapshot {
            processes: Vec::new(),
            error: Some("proc unavailable".into()),
        })));
        assert_eq!(app.process_summary(), summary);
        assert_eq!(app.process_error(), Some("proc unavailable"));
        assert!(app.update(Action::ProcessesUpdated(process_snapshot.clone())));
        assert_eq!(app.process_error(), None);

        assert!(app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: Vec::new(),
            error: Some("net unavailable".into()),
        })));
        assert_eq!(app.network_count(), 1);
        assert_eq!(app.network_error(), Some("net unavailable"));
        assert!(app.update(Action::NetworkUpdated(network_snapshot.clone())));
        assert_eq!(app.network_error(), None);

        app.update(Action::SelectTab(Tab::Services));
        assert!(!app.update(Action::ProcessesUpdated(ProcessSnapshot {
            processes: Vec::new(),
            error: Some("proc unavailable".into()),
        })));
        assert!(!app.update(Action::ProcessesUpdated(process_snapshot)));
        assert!(!app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: Vec::new(),
            error: Some("net unavailable".into()),
        })));
        assert!(!app.update(Action::NetworkUpdated(network_snapshot)));
        assert_eq!(app.process_error(), None);
        assert_eq!(app.network_error(), None);
    }

    #[test]
    fn aggregate_cpu_history_is_bounded_and_evicts_oldest_samples() {
        let mut app = App::default();
        let sample_count = AGGREGATE_CPU_HISTORY_CAPACITY + 5;

        for sample in 0..sample_count {
            app.update(Action::SystemMetricsUpdated(SystemMetrics {
                cpu_percent: Some(sample as f64),
                ..Default::default()
            }));
        }

        let history = app.aggregate_cpu_history().iter().collect::<Vec<_>>();
        assert_eq!(history.len(), AGGREGATE_CPU_HISTORY_CAPACITY);
        assert_eq!(history.first(), Some(&5.0));
        assert_eq!(history.last(), Some(&64.0));
    }

    fn assert_resize_preserves_overlay(app: &mut App, overlay_is_open: impl Fn(&App) -> bool) {
        assert!(overlay_is_open(app));
        assert!(app.update(Action::HoverMouseTarget(Some(MouseTarget::Tab(
            Tab::Overview,
        )))));

        assert!(app.update(Action::Resize));

        assert!(overlay_is_open(app));
        assert_eq!(app.hovered(), None);
    }

    #[test]
    fn resize_is_global_and_preserves_every_overlay() {
        let mut app = App::default();
        app.update(Action::ShowHelp);
        assert_resize_preserves_overlay(&mut app, App::help_visible);

        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(
            1, "proc",
        )])));
        app.update(Action::OpenProcessDetails);
        assert_resize_preserves_overlay(&mut app, App::process_detail_visible);

        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(
            1, "proc",
        )])));
        app.update(Action::RequestProcessSignal(ProcessSignal::Term));
        assert_resize_preserves_overlay(&mut app, |app| {
            app.process_signal_confirmation().is_some()
        });

        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![service(
            "dbus.service",
            "active",
            "D-Bus System Message Bus",
        )])));
        app.update(Action::OpenServiceDetails);
        assert_resize_preserves_overlay(&mut app, App::service_detail_visible);

        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            1, "kernel", 6, "ready",
        )])));
        app.update(Action::OpenLogDetails);
        assert_resize_preserves_overlay(&mut app, App::log_detail_visible);

        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0")],
            error: None,
        }));
        app.update(Action::OpenNetworkDetails);
        assert_resize_preserves_overlay(&mut app, App::network_detail_visible);
    }

    #[test]
    fn resize_is_global_in_search_input_modes() {
        for (tab, begin_search, expected_mode) in [
            (
                Tab::Processes,
                Action::BeginProcessSearch,
                InputMode::ProcessSearch,
            ),
            (
                Tab::Services,
                Action::BeginServiceSearch,
                InputMode::ServiceSearch,
            ),
            (Tab::Logs, Action::BeginLogSearch, InputMode::LogSearch),
        ] {
            let mut app = App::default();
            app.update(Action::SelectTab(tab));
            app.update(begin_search);
            app.update(Action::HoverMouseTarget(Some(MouseTarget::Tab(tab))));

            assert!(app.update(Action::Resize));
            assert_eq!(app.input_mode(), expected_mode);
            assert_eq!(app.hovered(), None);
        }
    }

    #[test]
    fn escape_clears_search_only_for_active_tab() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('f'));
        assert_eq!(app.process_search_query(), "f");

        app.update(Action::SelectTab(Tab::Services));
        assert_eq!(app.process_search_query(), "f");
        app.update(Action::BeginServiceSearch);
        app.update(Action::AppendServiceSearch('s'));
        assert_eq!(app.service_search_query(), "s");

        // Esc on Services tab should clear Services search, not Processes search
        app.update(Action::Escape);
        assert_eq!(app.service_search_query(), "");
        assert!(!app.service_searching());
        assert_eq!(app.process_search_query(), "f");
    }
}
