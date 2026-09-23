//! Central application state: the `App` struct, action dispatch and tab/overlay handling.
//! Per-screen state transitions live in the sibling modules.

use std::{
    cmp::Ordering,
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::{
    action::{
        Action, InputMode, IntervalStep, MenuItem, MouseTarget, PinMove, ProcessSort,
        ProcessSortField, SignalConfirmButton, Tab,
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

use health::CollectorHealth;
pub use health::{Collector, CollectorPeriods, DEFAULT_SAMPLING_INTERVAL, SAMPLING_PRESETS};
use logs::LogView;
pub(crate) use network::{is_overview_interface, is_physical_interface};
use processes::{PinnedProcess, ProcessKeys, ProcessRow, ProcessView};
use services::ServiceView;

const LOG_BUFFER_CAPACITY: usize = 2_000;
const METRIC_HISTORY_CAPACITY: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSignalConfirmation {
    pub identity: ProcessIdentity,
    pub name: String,
    pub signal: ProcessSignal,
    pub focused_button: SignalConfirmButton,
}

/// The last `METRIC_HISTORY_CAPACITY` samples of one metric (CPU %, RAM %,
/// network bytes/s), one per sampling interval. The buffer is allocated once
/// and never grows.
#[derive(Debug)]
pub struct MetricHistory {
    samples: VecDeque<f64>,
}

impl Default for MetricHistory {
    fn default() -> Self {
        Self {
            samples: VecDeque::with_capacity(METRIC_HISTORY_CAPACITY),
        }
    }
}

impl MetricHistory {
    /// Records a sample; non-finite or negative values are rejected.
    fn push(&mut self, value: f64) -> bool {
        if !value.is_finite() || value < 0.0 {
            return false;
        }
        if self.samples.len() == METRIC_HISTORY_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
        true
    }

    fn push_percent(&mut self, percent: f64) -> bool {
        percent.is_finite() && self.push(percent.clamp(0.0, 100.0))
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    /// Number of samples kept; the history covers `capacity × sampling interval`.
    pub fn capacity(&self) -> usize {
        METRIC_HISTORY_CAPACITY
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = f64> + ExactSizeIterator + '_ {
        self.samples.iter().copied()
    }
}

/// The modal layer shown above the active screen; at most one is open at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    /// The main menu, opened by `Esc` when there is nothing else to close.
    Menu {
        selected: MenuItem,
    },
    /// Opened from the menu; `Esc` returns to the menu.
    About,
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
    aggregate_cpu_history: MetricHistory,
    memory_history: MetricHistory,
    /// Combined RX+TX bytes/s of the interfaces the Overview lists.
    network_history: MetricHistory,
    processes: Vec<ProcessInfo>,
    /// Lowercased search/sort keys parallel to `processes`; empty until the next
    /// filter rebuild after a snapshot replaced `processes`.
    process_keys: Vec<ProcessKeys>,
    process_summary: ProcessSummary,
    /// Rows of the Processes table: the pinned section, then the filtered and
    /// sorted unpinned processes.
    filtered_processes: Vec<ProcessRow>,
    /// Pinned processes in the user's order; at most `MAX_PINNED_PROCESSES`.
    pinned: Vec<PinnedProcess>,
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
    process_view: ProcessView,
    process_action_message: Option<String>,
    services: Vec<ServiceInfo>,
    filtered_services: Vec<usize>,
    selected_service: Option<String>,
    service_scroll: usize,
    service_view_height: usize,
    service_search_query: String,
    service_searching: bool,
    service_view: ServiceView,
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
    log_view: LogView,
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
    cpu_history_interval: Duration,
    sampling_interval: Duration,
    sampling_interval_changed: bool,
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
            aggregate_cpu_history: MetricHistory::default(),
            memory_history: MetricHistory::default(),
            network_history: MetricHistory::default(),
            processes: Vec::new(),
            process_keys: Vec::new(),
            process_summary: ProcessSummary::default(),
            filtered_processes: Vec::new(),
            pinned: Vec::new(),
            deferred_process_rebuild: None,
            selected_process: None,
            process_scroll: 0,
            process_view_height: 0,
            process_search_query: String::new(),
            process_searching: false,
            process_error: None,
            process_sort: ProcessSort::default(),
            process_view: ProcessView::All,
            process_action_message: None,
            services: Vec::new(),
            filtered_services: Vec::new(),
            selected_service: None,
            service_scroll: 0,
            service_view_height: 0,
            service_search_query: String::new(),
            service_searching: false,
            service_view: ServiceView::All,
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
            log_view: LogView::All,
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
            cpu_history_interval: DEFAULT_SAMPLING_INTERVAL,
            sampling_interval: DEFAULT_SAMPLING_INTERVAL,
            sampling_interval_changed: false,
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

    /// The highlighted item while the main menu is open.
    pub fn menu_selection(&self) -> Option<MenuItem> {
        match self.overlay {
            Some(Overlay::Menu { selected }) => Some(selected),
            _ => None,
        }
    }

    pub fn about_visible(&self) -> bool {
        self.overlay == Some(Overlay::About)
    }

    pub fn system_metrics(&self) -> &SystemMetrics {
        &self.system_metrics
    }

    pub fn aggregate_cpu_history(&self) -> &MetricHistory {
        &self.aggregate_cpu_history
    }

    pub fn memory_history(&self) -> &MetricHistory {
        &self.memory_history
    }

    pub fn network_history(&self) -> &MetricHistory {
        &self.network_history
    }

    pub fn hardware(&self) -> Option<&HardwareInventory> {
        self.hardware.as_ref()
    }

    pub fn input_mode(&self) -> InputMode {
        if let Some(overlay) = &self.overlay {
            match overlay {
                Overlay::Help => InputMode::Help,
                Overlay::Menu { .. } => InputMode::Menu,
                Overlay::About => InputMode::About,
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
                let cpu_recorded = metrics
                    .cpu_percent
                    .is_some_and(|sample| self.aggregate_cpu_history.push_percent(sample));
                let memory_recorded = metrics
                    .memory
                    .is_some_and(|memory| self.memory_history.push_percent(memory.percent()));
                let history_changed = cpu_recorded || memory_recorded;
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
            Action::Tick(now) => self.check_collector_staleness(now) | self.expire_exited_pins(now),
            Action::Escape => self.escape(),
            Action::CancelProcessSignal => self.cancel_process_signal(),
            Action::ConfirmProcessSignal => self.confirm_process_signal(),
            Action::ToggleProcessSignalFocus => self.toggle_process_signal_focus(),
            Action::FocusProcessSignal(button) => self.focus_process_signal(button),
            Action::ExecuteFocusedProcessSignal => self.execute_focused_process_signal(),
            Action::MenuPrevious => self.move_menu_selection(-1),
            Action::MenuNext => self.move_menu_selection(1),
            Action::ActivateSelectedMenuItem => match self.menu_selection() {
                Some(item) => self.activate_menu_item(item),
                None => false,
            },
            Action::ActivateMenuItem(item) => self.activate_menu_item(item),
            Action::Resize => unreachable!("resize actions return before modal suppression"),
            _ if self.overlay.is_some() => false,
            Action::ShowHelp => {
                self.overlay = Some(Overlay::Help);
                self.hovered = None;
                true
            }
            Action::StepSamplingInterval(step) => self.step_sampling_interval(step),
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
            Action::TogglePin => self.toggle_selected_pin(),
            Action::CycleViewFilter => match self.active_tab {
                Tab::Processes => self.cycle_process_view(),
                Tab::Services => self.cycle_service_view(),
                Tab::Logs => self.cycle_log_view(),
                Tab::Overview | Tab::Network => false,
            },
            Action::MoveSelectedPin(direction) => self.move_selected_pin(direction),
            Action::MovePin(identity, direction) => self.move_pin(identity, direction),
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

    /// Esc order: close the overlay (About goes back to the menu), else clear
    /// the tab's search, else its view filter, else open the main menu.
    fn escape(&mut self) -> bool {
        if matches!(self.overlay, Some(Overlay::ProcessSignal(_))) {
            self.cancel_process_signal()
        } else if self.overlay == Some(Overlay::About) {
            self.overlay = Some(Overlay::Menu {
                selected: MenuItem::About,
            });
            true
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
                Tab::Processes if self.process_view != ProcessView::All => {
                    self.process_view = ProcessView::All;
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
                Tab::Services if self.service_view != ServiceView::All => {
                    self.service_view = ServiceView::All;
                    self.rebuild_service_filter();
                    true
                }
                Tab::Logs if self.log_searching || !self.log_search_query.is_empty() => {
                    self.log_searching = false;
                    self.log_search_query.clear();
                    self.rebuild_log_filter();
                    true
                }
                Tab::Logs if self.log_view != LogView::All => {
                    self.log_view = LogView::All;
                    self.rebuild_log_filter();
                    true
                }
                _ => {
                    self.overlay = Some(Overlay::Menu {
                        selected: MenuItem::About,
                    });
                    self.hovered = None;
                    true
                }
            }
        }
    }

    fn move_menu_selection(&mut self, delta: isize) -> bool {
        let Some(Overlay::Menu { selected }) = &mut self.overlay else {
            return false;
        };
        let current = MenuItem::ALL
            .iter()
            .position(|item| item == selected)
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(MenuItem::ALL.len() - 1);
        let changed = MenuItem::ALL[next] != *selected;
        *selected = MenuItem::ALL[next];
        changed
    }

    fn activate_menu_item(&mut self, item: MenuItem) -> bool {
        if self.menu_selection().is_none() {
            return false;
        }
        match item {
            MenuItem::About => {
                self.overlay = Some(Overlay::About);
                self.hovered = None;
                true
            }
            MenuItem::Exit => {
                // Not destructive: leaving tuxctl needs no confirmation.
                self.should_quit = true;
                false
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
        Menu,
        About,
    }

    const OVERLAYS: [OverlayKind; 8] = [
        OverlayKind::Help,
        OverlayKind::ProcessDetail,
        OverlayKind::ProcessSignal,
        OverlayKind::ServiceDetail,
        OverlayKind::LogDetail,
        OverlayKind::NetworkDetail,
        OverlayKind::Menu,
        OverlayKind::About,
    ];

    /// The input mode of a tab with no overlay and no search in progress.
    fn base_input_mode(tab: Tab) -> InputMode {
        match tab {
            Tab::Services => InputMode::Services,
            Tab::Logs => InputMode::Logs,
            Tab::Network => InputMode::Network,
            Tab::Overview | Tab::Processes => InputMode::Normal,
        }
    }

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
            OverlayKind::Menu => (Tab::Overview, Action::Escape),
            OverlayKind::About => {
                app.update(Action::Escape);
                (Tab::Overview, Action::ActivateMenuItem(MenuItem::About))
            }
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

    /// Esc order: close the overlay, else clear the tab's search, else its
    /// view filter, else open the main menu; Esc then closes the menu.
    #[test]
    fn escape_with_nothing_to_close_opens_the_menu_on_every_tab() {
        for tab in Tab::ALL {
            let mut app = app_with_overlay(OverlayKind::Help);
            app.update(Action::Escape);
            app.update(Action::SelectTab(tab));

            assert!(app.update(Action::Escape), "{tab:?}");
            assert_eq!(app.menu_selection(), Some(MenuItem::About), "{tab:?}");
            assert_eq!(app.active_tab(), tab);

            assert!(app.update(Action::Escape), "{tab:?} closes the menu");
            assert!(app.overlay.is_none());
        }
    }

    #[test]
    fn escape_clears_search_input_then_filter_on_searchable_tabs() {
        let searches = [
            (
                Tab::Processes,
                Action::BeginProcessSearch,
                Action::AppendProcessSearch('i'),
                Action::OpenProcessDetails,
            ),
            (
                Tab::Services,
                Action::BeginServiceSearch,
                Action::AppendServiceSearch('s'),
                Action::OpenServiceDetails,
            ),
            (
                Tab::Logs,
                Action::BeginLogSearch,
                Action::AppendLogSearch('b'),
                Action::OpenLogDetails,
            ),
        ];
        for (tab, begin, append, open_details) in searches {
            let mut app = app_with_overlay(OverlayKind::Help);
            app.update(Action::Escape);
            app.update(Action::SelectTab(tab));

            // Typing a query: Esc cancels input and clears the query.
            app.update(begin.clone());
            app.update(append.clone());
            assert!(app.update(Action::Escape), "{tab:?} search input");
            assert_eq!(app.input_mode(), base_input_mode(tab), "{tab:?}");

            // A committed filter (detail opened, then closed): Esc clears it next.
            app.update(begin);
            app.update(append);
            assert!(app.update(open_details), "{tab:?} details");
            assert!(app.update(Action::Escape), "{tab:?} closes details first");
            assert!(app.overlay.is_none());
            assert!(app.update(Action::Escape), "{tab:?} clears the filter");
            assert!(app.menu_selection().is_none(), "{tab:?}");
            assert!(app.update(Action::Escape), "{tab:?} opens the menu last");
            assert!(app.menu_selection().is_some(), "{tab:?}");
        }
    }

    /// Arrow keys during search move through the matches, the query stays
    /// active, and Enter opens the row the user moved to.
    #[test]
    fn navigating_during_search_chooses_the_row_enter_opens() {
        let mut app = App::default();
        app.update(Action::ProcessesUpdated(processes(vec![
            process(1, "sshd"),
            process(2, "bash"),
            process(3, "ssh-agent"),
        ])));
        app.update(Action::ServicesUpdated(services(vec![
            service("sshd.service", "active", "OpenSSH"),
            service("dbus.service", "active", "D-Bus"),
            service("ssh-agent.service", "active", "Agent"),
        ])));
        app.update(Action::LogsUpdated(log_batch(vec![
            log_entry(1, "sshd", 6, "ssh accepted"),
            log_entry(2, "kernel", 6, "boot"),
            log_entry(3, "sshd", 6, "ssh closed"),
        ])));

        let cases = [
            (
                Tab::Processes,
                Action::BeginProcessSearch,
                Action::AppendProcessSearch('s'),
                Action::ProcessNext,
                Action::OpenProcessDetails,
            ),
            (
                Tab::Services,
                Action::BeginServiceSearch,
                Action::AppendServiceSearch('s'),
                Action::ServiceNext,
                Action::OpenServiceDetails,
            ),
            // Following logs select the newest match, so the move is upwards.
            (
                Tab::Logs,
                Action::BeginLogSearch,
                Action::AppendLogSearch('s'),
                Action::LogPrevious,
                Action::OpenLogDetails,
            ),
        ];
        let selected = |app: &App, tab: Tab| match tab {
            Tab::Processes => app.selected_process().map(|p| p.name.clone()),
            Tab::Services => app.selected_service().map(|s| s.unit.clone()),
            _ => app.selected_log().map(|entry| entry.id.to_string()),
        };
        for (tab, begin, append, step, open) in cases {
            app.update(Action::SelectTab(tab));
            app.update(begin);
            app.update(append.clone());
            app.update(append);
            let searching = app.input_mode();
            let before = selected(&app, tab);

            assert!(app.update(step), "{tab:?} moved");
            let chosen = selected(&app, tab);
            assert_ne!(chosen, before, "{tab:?}");
            assert_eq!(app.input_mode(), searching, "{tab:?} search stays active");

            assert!(app.update(open), "{tab:?} opens details");
            assert_eq!(selected(&app, tab), chosen, "{tab:?}");
            app.update(Action::Escape);
        }
        assert_eq!(app.process_search_query(), "ss");
    }

    #[test]
    fn navigating_an_empty_search_result_is_a_no_op() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(processes(vec![process(
            1, "init",
        )])));
        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('z'));

        for action in [
            Action::ProcessNext,
            Action::ProcessPrevious,
            Action::ProcessNextPage,
            Action::ProcessPreviousPage,
        ] {
            assert!(!app.update(action));
        }
        assert!(!app.update(Action::OpenProcessDetails));
    }

    #[test]
    fn escape_clears_the_search_then_the_view_filter() {
        let cases = [
            (
                Tab::Processes,
                Action::BeginProcessSearch,
                Action::AppendProcessSearch('i'),
            ),
            (
                Tab::Services,
                Action::BeginServiceSearch,
                Action::AppendServiceSearch('s'),
            ),
            (
                Tab::Logs,
                Action::BeginLogSearch,
                Action::AppendLogSearch('b'),
            ),
        ];
        for (tab, begin, append) in cases {
            let mut app = app_with_overlay(OverlayKind::Help);
            app.update(Action::Escape);
            app.update(Action::SelectTab(tab));
            assert!(app.update(Action::CycleViewFilter), "{tab:?}");
            app.update(begin);
            app.update(append);
            let view = |app: &App| match tab {
                Tab::Processes => app.process_view_label(),
                Tab::Services => app.service_view_label(),
                _ => app.log_view_label(),
            };
            assert!(view(&app).is_some(), "{tab:?}");

            assert!(app.update(Action::Escape), "{tab:?} clears the search");
            assert!(view(&app).is_some(), "{tab:?} keeps the view");
            assert!(app.update(Action::Escape), "{tab:?} clears the view");
            assert!(view(&app).is_none(), "{tab:?}");
            assert!(app.menu_selection().is_none(), "{tab:?}");
            assert!(app.update(Action::Escape), "{tab:?} opens the menu last");
            assert!(app.menu_selection().is_some(), "{tab:?}");
        }
    }

    #[test]
    fn view_filters_cycle_only_on_their_screen() {
        let mut app = App::default();
        for tab in [Tab::Overview, Tab::Network] {
            app.update(Action::SelectTab(tab));
            assert!(!app.update(Action::CycleViewFilter), "{tab:?}");
        }
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ShowHelp);
        assert!(
            !app.update(Action::CycleViewFilter),
            "blocked by an overlay"
        );
    }

    #[test]
    fn escape_closes_exactly_one_overlay_per_press() {
        // About goes back to the menu instead (tested below).
        for kind in OVERLAYS
            .into_iter()
            .filter(|kind| !matches!(kind, OverlayKind::About))
        {
            let mut app = app_with_overlay(kind);

            assert!(app.update(Action::Escape), "{kind:?}");
            assert!(app.overlay.is_none(), "{kind:?}");
            // With nothing left to close, the next Esc opens the menu.
            assert!(app.update(Action::Escape), "{kind:?}");
            assert!(app.menu_selection().is_some(), "{kind:?}");
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
    fn the_menu_moves_between_items_and_opens_about() {
        let mut app = App::default();
        assert!(!app.update(Action::MenuNext), "no menu open");

        assert!(app.update(Action::Escape));
        assert_eq!(app.input_mode(), InputMode::Menu);
        assert!(!app.update(Action::MenuPrevious), "already at the top");
        assert!(app.update(Action::MenuNext));
        assert_eq!(app.menu_selection(), Some(MenuItem::Exit));
        assert!(!app.update(Action::MenuNext), "already at the bottom");
        assert!(app.update(Action::MenuPrevious));

        assert!(app.update(Action::ActivateSelectedMenuItem));
        assert!(app.about_visible());
        assert_eq!(app.input_mode(), InputMode::About);

        assert!(app.update(Action::Escape), "About goes back to the menu");
        assert_eq!(app.menu_selection(), Some(MenuItem::About));
        assert!(app.update(Action::Escape));
        assert!(app.overlay.is_none());
        assert!(!app.should_quit());
    }

    #[test]
    fn exit_quits_without_confirmation_but_only_from_the_menu() {
        let mut app = App::default();
        // A stale click on where the menu was must not quit.
        app.update(Action::ActivateMenuItem(MenuItem::Exit));
        assert!(!app.should_quit());

        app.update(Action::Escape);
        app.update(Action::MenuNext);
        app.update(Action::ActivateSelectedMenuItem);
        assert!(app.should_quit());

        let mut app = App::default();
        app.update(Action::Escape);
        app.update(Action::ActivateMenuItem(MenuItem::Exit));
        assert!(app.should_quit());
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
    fn metric_history_never_grows_and_rejects_invalid_samples() {
        let mut history = MetricHistory::default();
        let capacity = history.samples.capacity();

        for sample in 0..(METRIC_HISTORY_CAPACITY * 3) {
            assert!(history.push(sample as f64));
        }
        assert!(!history.push(f64::NAN));
        assert!(!history.push(f64::INFINITY));
        assert!(!history.push(-1.0));
        assert!(history.push_percent(250.0), "percentages are clamped");

        assert_eq!(history.iter().len(), METRIC_HISTORY_CAPACITY);
        assert_eq!(history.iter().last(), Some(100.0));
        assert_eq!(history.samples.capacity(), capacity, "no reallocation");
    }

    #[test]
    fn memory_usage_is_recorded_with_each_metrics_sample() {
        let mut app = App::default();
        for used in [1, 2, 3] {
            app.update(Action::SystemMetricsUpdated(SystemMetrics {
                memory: Some(crate::linux::ByteUsage { used, total: 4 }),
                ..SystemMetrics::default()
            }));
        }
        assert_eq!(
            app.memory_history().iter().collect::<Vec<_>>(),
            [25.0, 50.0, 75.0]
        );
    }

    #[test]
    fn network_history_sums_the_overview_interfaces_once_per_snapshot() {
        let mut app = App::default();
        let interface = |name: &str, rx, tx| NetworkInterfaceInfo {
            rx_rate_bytes_per_sec: rx,
            tx_rate_bytes_per_sec: tx,
            ..dummy_network(name)
        };
        let snapshot = NetworkSnapshot {
            interfaces: vec![
                interface("enp6s0", Some(1000.0), Some(24.0)),
                interface("wlan0", Some(1.0), None),
                interface("lo", Some(5000.0), Some(5000.0)),
                interface("docker0", Some(700.0), Some(700.0)),
                interface("veth12ab", Some(700.0), Some(700.0)),
            ],
            error: None,
        };

        assert!(app.update(Action::NetworkUpdated(snapshot.clone())));
        // An unchanged snapshot still adds a sample and redraws the Overview.
        assert!(app.update(Action::NetworkUpdated(snapshot.clone())));
        assert_eq!(
            app.network_history().iter().collect::<Vec<_>>(),
            [1025.0, 1025.0]
        );

        // Errors and snapshots without any rate add nothing.
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: Vec::new(),
            error: Some("read failed".into()),
        }));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![interface("enp6s0", None, None)],
            error: None,
        }));
        assert_eq!(app.network_history().iter().len(), 2);

        // Hidden from the Overview, a sample is still kept but redraws nothing.
        app.update(Action::SelectTab(Tab::Logs));
        assert!(!app.update(Action::NetworkUpdated(snapshot)));
        assert_eq!(app.network_history().iter().len(), 3);
    }

    #[test]
    fn aggregate_cpu_history_is_bounded_and_evicts_oldest_samples() {
        let mut app = App::default();
        let sample_count = METRIC_HISTORY_CAPACITY + 5;

        for sample in 0..sample_count {
            app.update(Action::SystemMetricsUpdated(SystemMetrics {
                cpu_percent: Some(sample as f64),
                ..Default::default()
            }));
        }

        let history = app.aggregate_cpu_history().iter().collect::<Vec<_>>();
        assert_eq!(history.len(), METRIC_HISTORY_CAPACITY);
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
