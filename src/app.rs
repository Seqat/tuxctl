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

const LOG_BUFFER_CAPACITY: usize = 2_000;
const AGGREGATE_CPU_HISTORY_CAPACITY: usize = 60;

/// Background collectors whose last update time is tracked for the stale marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collector {
    Metrics,
    Processes,
    Network,
    Services,
}

impl Collector {
    const ALL: [Self; 4] = [
        Self::Metrics,
        Self::Processes,
        Self::Network,
        Self::Services,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Metrics => "metrics",
            Self::Processes => "processes",
            Self::Network => "network",
            Self::Services => "services",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    fn shown_on(self, tab: Tab) -> bool {
        match self {
            Self::Metrics | Self::Processes => matches!(tab, Tab::Overview | Tab::Processes),
            Self::Network => matches!(tab, Tab::Overview | Tab::Network),
            Self::Services => tab == Tab::Services,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct CollectorHealth {
    /// Disabled until [`App::with_collector_periods`] configures a threshold.
    stale_after: Option<Duration>,
    /// Tick time at which data last arrived; the first tick is the baseline so a
    /// collector that never publishes still becomes stale.
    last_update: Option<Instant>,
    updated_since_tick: bool,
    stale: bool,
}

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

#[derive(Debug)]
pub struct App {
    should_quit: bool,
    active_tab: Tab,
    help_visible: bool,
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
    process_detail_visible: bool,
    process_error: Option<String>,
    process_sort: ProcessSort,
    process_signal_confirmation: Option<ProcessSignalConfirmation>,
    process_action_message: Option<String>,
    services: Vec<ServiceInfo>,
    filtered_services: Vec<usize>,
    selected_service: Option<String>,
    service_scroll: usize,
    service_view_height: usize,
    service_search_query: String,
    service_searching: bool,
    service_detail_visible: bool,
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
    log_detail_visible: bool,
    log_following: bool,
    log_paused: bool,
    log_dropped: usize,
    log_error: Option<String>,
    networks: Vec<NetworkInterfaceInfo>,
    selected_network: Option<String>,
    network_scroll: usize,
    network_view_height: usize,
    network_detail_visible: bool,
    network_error: Option<String>,
    hovered: Option<MouseTarget>,
    collector_health: [CollectorHealth; 4],
}

impl Default for App {
    fn default() -> Self {
        Self {
            should_quit: false,
            active_tab: Tab::Overview,
            help_visible: false,
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
            process_detail_visible: false,
            process_error: None,
            process_sort: ProcessSort::default(),
            process_signal_confirmation: None,
            process_action_message: None,
            services: Vec::new(),
            filtered_services: Vec::new(),
            selected_service: None,
            service_scroll: 0,
            service_view_height: 0,
            service_search_query: String::new(),
            service_searching: false,
            service_detail_visible: false,
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
            log_detail_visible: false,
            log_following: true,
            log_paused: false,
            log_dropped: 0,
            log_error: None,
            networks: Vec::new(),
            selected_network: None,
            network_scroll: 0,
            network_view_height: 0,
            network_detail_visible: false,
            network_error: None,
            hovered: None,
            collector_health: [CollectorHealth::default(); 4],
        }
    }
}

impl App {
    /// Enables stale markers: a collector is stale after three missed periods.
    /// Services also allow for a full `systemctl` timeout before being flagged.
    pub fn with_collector_periods(mut self, sampling: Duration, services: Duration) -> Self {
        for collector in Collector::ALL {
            let threshold = match collector {
                Collector::Services => services.saturating_mul(2) + SYSTEMCTL_TIMEOUT,
                _ => sampling.saturating_mul(3),
            };
            self.collector_health[collector.index()].stale_after = Some(threshold);
        }
        self
    }

    /// Stale collectors whose data is shown on the active tab.
    pub fn stale_collectors(&self) -> impl Iterator<Item = Collector> + '_ {
        Collector::ALL.into_iter().filter(|collector| {
            self.collector_health[collector.index()].stale && collector.shown_on(self.active_tab)
        })
    }

    fn record_collector_update(&mut self, collector: Collector) {
        self.collector_health[collector.index()].updated_since_tick = true;
    }

    /// Returns `true` only when a collector shown on the active tab changes
    /// between fresh and stale.
    fn check_collector_staleness(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        for collector in Collector::ALL {
            let health = &mut self.collector_health[collector.index()];
            let Some(stale_after) = health.stale_after else {
                continue;
            };
            if std::mem::take(&mut health.updated_since_tick) {
                health.last_update = Some(now);
            }
            let last_update = *health.last_update.get_or_insert(now);
            let stale = now.saturating_duration_since(last_update) > stale_after;
            if stale != health.stale {
                health.stale = stale;
                redraw |= collector.shown_on(self.active_tab);
            }
        }
        redraw
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn active_tab(&self) -> Tab {
        self.active_tab
    }

    pub fn help_visible(&self) -> bool {
        self.help_visible
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
        if self.help_visible {
            InputMode::Help
        } else if self.process_signal_confirmation.is_some() {
            InputMode::ProcessSignalConfirm
        } else if self.process_detail_visible {
            InputMode::ProcessDetail
        } else if self.service_detail_visible {
            InputMode::ServiceDetail
        } else if self.log_detail_visible {
            InputMode::LogDetail
        } else if self.network_detail_visible {
            InputMode::NetworkDetail
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
        self.process_detail_visible
    }

    pub fn process_error(&self) -> Option<&str> {
        self.process_error.as_deref()
    }

    pub fn process_sort(&self) -> ProcessSort {
        self.process_sort
    }

    pub fn process_signal_confirmation(&self) -> Option<&ProcessSignalConfirmation> {
        self.process_signal_confirmation.as_ref()
    }

    pub fn process_action_message(&self) -> Option<&str> {
        self.process_action_message.as_deref()
    }

    pub fn service_count(&self) -> usize {
        self.filtered_services.len()
    }

    pub fn service_at(&self, index: usize) -> Option<&ServiceInfo> {
        self.filtered_services
            .get(index)
            .and_then(|index| self.services.get(*index))
    }

    pub fn selected_service_index(&self) -> Option<usize> {
        let selected = self.selected_service.as_deref()?;
        self.filtered_services.iter().position(|index| {
            self.services
                .get(*index)
                .is_some_and(|service| service.unit == selected)
        })
    }

    pub fn selected_service(&self) -> Option<&ServiceInfo> {
        let selected = self.selected_service.as_deref()?;
        self.services
            .iter()
            .find(|service| service.unit == selected)
    }

    pub fn service_scroll(&self) -> usize {
        self.service_scroll
    }

    pub fn service_search_query(&self) -> &str {
        &self.service_search_query
    }

    pub fn service_searching(&self) -> bool {
        self.service_searching
    }

    pub fn service_detail_visible(&self) -> bool {
        self.service_detail_visible
    }

    pub fn service_error(&self) -> Option<&str> {
        self.service_error.as_deref()
    }

    pub fn service_refreshing(&self) -> bool {
        self.pending_service_refresh_generation.is_some()
    }

    pub fn take_service_refresh_request(&mut self) -> Option<ServiceRefreshGeneration> {
        self.service_refresh_requested.take()
    }

    pub fn log_count(&self) -> usize {
        self.filtered_logs.len()
    }

    pub fn log_at(&self, index: usize) -> Option<&JournalEntry> {
        self.filtered_logs
            .get(index)
            .and_then(|index| self.logs.get(*index))
    }

    pub fn selected_log_index(&self) -> Option<usize> {
        let selected = self.selected_log?;
        self.filtered_logs.iter().position(|index| {
            self.logs
                .get(*index)
                .is_some_and(|entry| entry.id == selected)
        })
    }

    pub fn selected_log(&self) -> Option<&JournalEntry> {
        let selected = self.selected_log?;
        self.logs.iter().find(|entry| entry.id == selected)
    }

    pub fn log_scroll(&self) -> usize {
        self.log_scroll
    }

    pub fn log_search_query(&self) -> &str {
        &self.log_search_query
    }

    pub fn log_searching(&self) -> bool {
        self.log_searching
    }

    pub fn log_detail_visible(&self) -> bool {
        self.log_detail_visible
    }

    pub fn log_following(&self) -> bool {
        self.log_following
    }

    pub fn log_paused(&self) -> bool {
        self.log_paused
    }

    pub fn log_dropped(&self) -> usize {
        self.log_dropped
    }

    pub fn log_error(&self) -> Option<&str> {
        self.log_error.as_deref()
    }

    pub fn network_count(&self) -> usize {
        self.networks.len()
    }

    pub fn networks(&self) -> &[NetworkInterfaceInfo] {
        &self.networks
    }

    pub fn network_at(&self, index: usize) -> Option<&NetworkInterfaceInfo> {
        self.networks.get(index)
    }

    pub fn selected_network_index(&self) -> Option<usize> {
        let selected = self.selected_network.as_deref()?;
        self.networks.iter().position(|net| net.name == selected)
    }

    pub fn selected_network(&self) -> Option<&NetworkInterfaceInfo> {
        let selected = self.selected_network.as_deref()?;
        self.networks.iter().find(|net| net.name == selected)
    }

    pub fn network_scroll(&self) -> usize {
        self.network_scroll
    }

    pub fn network_detail_visible(&self) -> bool {
        self.network_detail_visible
    }

    pub fn network_error(&self) -> Option<&str> {
        self.network_error.as_deref()
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
            _ if self.help_visible
                || self.process_signal_confirmation.is_some()
                || self.process_detail_visible
                || self.service_detail_visible
                || self.log_detail_visible
                || self.network_detail_visible =>
            {
                false
            }
            Action::ShowHelp => {
                self.help_visible = true;
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

    fn select_tab(&mut self, tab: Tab) -> bool {
        let changed = self.active_tab != tab
            || self.process_signal_confirmation.is_some()
            || self.process_searching
            || self.process_detail_visible
            || self.service_searching
            || self.service_detail_visible
            || self.log_searching
            || self.log_detail_visible
            || self.network_detail_visible
            || self.hovered.is_some();
        self.active_tab = tab;
        self.process_signal_confirmation = None;
        self.process_action_message = None;
        self.process_searching = false;
        self.process_detail_visible = false;
        self.service_searching = false;
        self.service_detail_visible = false;
        self.log_searching = false;
        self.log_detail_visible = false;
        self.network_detail_visible = false;
        self.hovered = None;
        if tab == Tab::Processes && self.deferred_process_rebuild.is_some() {
            self.rebuild_process_filter();
        }
        changed
    }

    fn escape(&mut self) -> bool {
        if self.help_visible {
            self.help_visible = false;
            true
        } else if self.process_signal_confirmation.is_some() {
            self.cancel_process_signal()
        } else if self.process_detail_visible {
            self.process_detail_visible = false;
            true
        } else if self.service_detail_visible {
            self.service_detail_visible = false;
            true
        } else if self.log_detail_visible {
            self.log_detail_visible = false;
            true
        } else if self.network_detail_visible {
            self.network_detail_visible = false;
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

    fn update_processes(&mut self, snapshot: ProcessSnapshot) -> bool {
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
            self.process_detail_visible = false;
        }
        self.reconcile_hovered_process();
        self.ensure_process_visible();
        processes_visible || overview_summary_changed || (overview_visible && was_stale)
    }

    fn close_confirmation_for_exited_process(&mut self) {
        if let Some(confirmation) = &self.process_signal_confirmation {
            if !self
                .processes
                .iter()
                .any(|p| p.identity() == confirmation.identity)
            {
                let name = confirmation.name.clone();
                let pid = confirmation.identity.pid;
                self.process_signal_confirmation = None;
                self.process_action_message =
                    Some(format!("Process {name} ({pid}) exited before signal"));
            }
        }
    }

    fn begin_process_search(&mut self) -> bool {
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

    fn append_process_search(&mut self, character: char) -> bool {
        if self.process_searching && !character.is_control() {
            self.process_search_query.push(character);
            self.rebuild_process_filter();
            true
        } else {
            false
        }
    }

    fn backspace_process_search(&mut self) -> bool {
        if self.process_searching && self.process_search_query.pop().is_some() {
            self.rebuild_process_filter();
            true
        } else {
            false
        }
    }

    fn rebuild_process_filter(&mut self) {
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

    fn move_process_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Processes || self.filtered_processes.is_empty() {
            return false;
        }

        let current = self.selected_process_index().unwrap_or(0);
        let last = self.filtered_processes.len() - 1;
        let next = current.saturating_add_signed(delta).min(last);
        self.select_process_index(next)
    }

    fn select_process_index(&mut self, index: usize) -> bool {
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

    fn select_process(&mut self, identity: ProcessIdentity) -> bool {
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

    fn open_process_details(&mut self) -> bool {
        if self.active_tab == Tab::Processes && self.selected_process().is_some() {
            self.process_searching = false;
            self.process_detail_visible = true;
            self.hovered = None;
            true
        } else {
            false
        }
    }

    fn request_process_signal(&mut self, signal: ProcessSignal) -> bool {
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
        self.process_signal_confirmation = Some(ProcessSignalConfirmation {
            identity,
            name,
            signal,
            focused_button: SignalConfirmButton::Cancel,
        });
        self.hovered = None;
        true
    }

    fn cancel_process_signal(&mut self) -> bool {
        if self.process_signal_confirmation.take().is_some() {
            self.hovered = None;
            true
        } else {
            false
        }
    }

    fn toggle_process_signal_focus(&mut self) -> bool {
        if let Some(confirmation) = &mut self.process_signal_confirmation {
            confirmation.focused_button = match confirmation.focused_button {
                SignalConfirmButton::Cancel => SignalConfirmButton::Confirm,
                SignalConfirmButton::Confirm => SignalConfirmButton::Cancel,
            };
            true
        } else {
            false
        }
    }

    fn focus_process_signal(&mut self, button: SignalConfirmButton) -> bool {
        if let Some(confirmation) = &mut self.process_signal_confirmation {
            if confirmation.focused_button != button {
                confirmation.focused_button = button;
                return true;
            }
        }
        false
    }

    fn execute_focused_process_signal(&mut self) -> bool {
        let Some(confirmation) = &self.process_signal_confirmation else {
            return false;
        };
        match confirmation.focused_button {
            SignalConfirmButton::Cancel => self.cancel_process_signal(),
            SignalConfirmButton::Confirm => self.confirm_process_signal(),
        }
    }

    fn confirm_process_signal(&mut self) -> bool {
        let Some(confirmation) = self.process_signal_confirmation.take() else {
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
        let Some(confirmation) = self.process_signal_confirmation.take() else {
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

    fn sort_processes(&mut self, field: ProcessSortField) -> bool {
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

    fn ensure_process_visible(&mut self) {
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

    fn update_services(&mut self, snapshot: ServiceSnapshot) -> bool {
        let visible = self.active_tab == Tab::Services;
        let was_refreshing = self.service_refreshing();
        if self
            .pending_service_refresh_generation
            .is_some_and(|pending| snapshot.completed_refresh_generation >= pending)
        {
            self.pending_service_refresh_generation = None;
        }
        let refresh_state_changed = was_refreshing != self.service_refreshing();
        if let Some(error) = snapshot.error {
            if self.service_error.as_ref() == Some(&error) {
                return refresh_state_changed && visible;
            }
            self.service_error = Some(error);
            return visible;
        }

        if self.service_error.is_none() && self.services == snapshot.services {
            return refresh_state_changed && visible;
        }

        let previous_index = self.selected_service_index().unwrap_or(0);
        let previous_selection = self.selected_service.clone();
        self.services = snapshot.services;
        self.service_error = None;
        self.rebuild_service_filter();

        self.selected_service = previous_selection.filter(|unit| {
            self.filtered_services.iter().any(|index| {
                self.services
                    .get(*index)
                    .is_some_and(|service| service.unit == *unit)
            })
        });
        if self.selected_service.is_none() {
            let replacement = previous_index.min(self.filtered_services.len().saturating_sub(1));
            self.selected_service = self
                .service_at(replacement)
                .map(|service| service.unit.clone());
            self.service_detail_visible = false;
        }
        self.reconcile_hovered_service();
        self.ensure_service_visible();
        visible
    }

    fn begin_service_search(&mut self) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        self.service_search_query.clear();
        self.service_searching = true;
        self.rebuild_service_filter();
        true
    }

    fn append_service_search(&mut self, character: char) -> bool {
        if !self.service_searching || character.is_control() {
            return false;
        }
        self.service_search_query.push(character);
        self.rebuild_service_filter();
        true
    }

    fn backspace_service_search(&mut self) -> bool {
        if self.service_searching && self.service_search_query.pop().is_some() {
            self.rebuild_service_filter();
            true
        } else {
            false
        }
    }

    fn rebuild_service_filter(&mut self) {
        let previous_index = self.selected_service_index().unwrap_or(0);
        let previous_selection = self.selected_service.clone();
        let query = self.service_search_query.to_lowercase();

        self.filtered_services = self
            .services
            .iter()
            .enumerate()
            .filter(|(_, service)| service_matches(service, &query))
            .map(|(index, _)| index)
            .collect();

        self.selected_service = previous_selection.filter(|unit| {
            self.filtered_services.iter().any(|index| {
                self.services
                    .get(*index)
                    .is_some_and(|service| service.unit == *unit)
            })
        });
        if self.selected_service.is_none() {
            let replacement = previous_index.min(self.filtered_services.len().saturating_sub(1));
            self.selected_service = self
                .service_at(replacement)
                .map(|service| service.unit.clone());
        }
        self.reconcile_hovered_service();
        self.ensure_service_visible();
    }

    fn move_service_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Services || self.filtered_services.is_empty() {
            return false;
        }
        let current = self.selected_service_index().unwrap_or(0);
        let last = self.filtered_services.len() - 1;
        self.select_service_index(current.saturating_add_signed(delta).min(last))
    }

    fn select_service_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        let Some(unit) = self.service_at(index).map(|service| service.unit.clone()) else {
            return false;
        };
        let previous = (self.selected_service.clone(), self.service_scroll);
        self.selected_service = Some(unit);
        self.ensure_service_visible();
        previous != (self.selected_service.clone(), self.service_scroll)
    }

    fn select_service(&mut self, unit: &str) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        let Some(index) = self.filtered_services.iter().position(|index| {
            self.services
                .get(*index)
                .is_some_and(|service| service.unit == unit)
        }) else {
            return false;
        };
        self.select_service_index(index)
    }

    fn open_service_details(&mut self) -> bool {
        if self.active_tab == Tab::Services && self.selected_service().is_some() {
            self.service_searching = false;
            self.service_detail_visible = true;
            self.hovered = None;
            true
        } else {
            false
        }
    }

    fn request_service_refresh(&mut self) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        self.service_refresh_generation = self.service_refresh_generation.saturating_add(1);
        let generation = self.service_refresh_generation;
        self.service_refresh_requested = Some(generation);
        let changed = !self.service_refreshing();
        self.pending_service_refresh_generation = Some(generation);
        changed
    }

    fn ensure_service_visible(&mut self) {
        self.service_scroll = calculate_scroll(
            self.filtered_services.len(),
            self.selected_service_index(),
            self.service_scroll,
            self.service_view_height,
        );
    }

    fn reconcile_hovered_service(&mut self) {
        if let Some(MouseTarget::ServiceRow(unit)) = &self.hovered {
            let remains_visible = self.filtered_services.iter().any(|index| {
                self.services
                    .get(*index)
                    .is_some_and(|service| service.unit == unit.as_ref())
            });
            if !remains_visible {
                self.hovered = None;
            }
        }
    }

    fn update_logs(&mut self, batch: JournalBatch) -> bool {
        let visible = self.active_tab == Tab::Logs;
        let mut changed = false;
        if batch.dropped > 0 {
            self.log_dropped = self.log_dropped.saturating_add(batch.dropped);
            changed = true;
        }
        if let Some(error) = batch.error {
            if self.log_error.as_ref() != Some(&error) {
                self.log_error = Some(error);
                changed = true;
            }
        }
        if batch.entries.is_empty() {
            return changed && visible;
        }

        let previous_index = self.selected_log_index().unwrap_or(0);
        let previous_selection = self.selected_log;
        for entry in batch.entries {
            if self.logs.len() == LOG_BUFFER_CAPACITY {
                self.logs.pop_front();
            }
            self.logs.push_back(entry);
        }
        self.rebuild_log_indices();
        self.restore_log_selection(previous_selection, previous_index);
        self.reconcile_hovered_log();
        self.ensure_log_visible();
        visible
    }

    fn begin_log_search(&mut self) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        self.log_search_query.clear();
        self.log_searching = true;
        self.rebuild_log_filter();
        true
    }

    fn append_log_search(&mut self, character: char) -> bool {
        if !self.log_searching || character.is_control() {
            return false;
        }
        self.log_search_query.push(character);
        self.rebuild_log_filter();
        true
    }

    fn backspace_log_search(&mut self) -> bool {
        if self.log_searching && self.log_search_query.pop().is_some() {
            self.rebuild_log_filter();
            true
        } else {
            false
        }
    }

    fn rebuild_log_filter(&mut self) {
        let previous_index = self.selected_log_index().unwrap_or(0);
        let previous_selection = self.selected_log;
        self.rebuild_log_indices();
        self.restore_log_selection(previous_selection, previous_index);
        self.reconcile_hovered_log();
        self.ensure_log_visible();
    }

    fn rebuild_log_indices(&mut self) {
        let query = self.log_search_query.to_lowercase();
        self.filtered_logs = self
            .logs
            .iter()
            .enumerate()
            .filter(|(_, entry)| log_matches(entry, &query))
            .map(|(index, _)| index)
            .collect();
    }

    fn restore_log_selection(&mut self, previous: Option<u64>, previous_index: usize) {
        if self.log_following && !self.log_paused {
            self.selected_log = self
                .log_at(self.filtered_logs.len().saturating_sub(1))
                .map(|entry| entry.id);
            return;
        }

        self.selected_log = previous.filter(|id| {
            self.filtered_logs
                .iter()
                .any(|index| self.logs.get(*index).is_some_and(|entry| entry.id == *id))
        });
        if self.selected_log.is_none() {
            let replacement = previous_index.min(self.filtered_logs.len().saturating_sub(1));
            self.selected_log = self.log_at(replacement).map(|entry| entry.id);
            self.log_detail_visible = false;
        }
    }

    fn move_log_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Logs || self.filtered_logs.is_empty() {
            return false;
        }
        let was_following = self.log_following;
        self.log_following = false;
        let current = self.selected_log_index().unwrap_or(0);
        let last = self.filtered_logs.len() - 1;
        self.set_log_index(current.saturating_add_signed(delta).min(last)) || was_following
    }

    fn select_log_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        let was_following = self.log_following;
        self.log_following = false;
        self.set_log_index(index) || was_following
    }

    fn set_log_index(&mut self, index: usize) -> bool {
        let Some(id) = self.log_at(index).map(|entry| entry.id) else {
            return false;
        };
        let previous = (self.selected_log, self.log_scroll);
        self.selected_log = Some(id);
        self.ensure_log_visible();
        previous != (self.selected_log, self.log_scroll)
    }

    fn select_log(&mut self, id: u64) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        let Some(index) = self
            .filtered_logs
            .iter()
            .position(|index| self.logs.get(*index).is_some_and(|entry| entry.id == id))
        else {
            return false;
        };
        let was_following = self.log_following;
        self.log_following = false;
        self.set_log_index(index) || was_following
    }

    fn open_log_details(&mut self) -> bool {
        if self.active_tab == Tab::Logs && self.selected_log().is_some() {
            self.log_searching = false;
            self.log_detail_visible = true;
            self.hovered = None;
            true
        } else {
            false
        }
    }

    fn toggle_log_follow(&mut self) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        self.log_following = !self.log_following;
        if self.log_following && !self.log_paused {
            let last = self.filtered_logs.len().saturating_sub(1);
            self.set_log_index(last);
        }
        true
    }

    fn toggle_log_pause(&mut self) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        self.log_paused = !self.log_paused;
        if !self.log_paused && self.log_following {
            let last = self.filtered_logs.len().saturating_sub(1);
            self.set_log_index(last);
        }
        true
    }

    fn ensure_log_visible(&mut self) {
        self.log_scroll = calculate_scroll(
            self.filtered_logs.len(),
            self.selected_log_index(),
            self.log_scroll,
            self.log_view_height,
        );
    }

    fn reconcile_hovered_log(&mut self) {
        if let Some(MouseTarget::LogRow(id)) = &self.hovered {
            if !self
                .filtered_logs
                .iter()
                .any(|index| self.logs.get(*index).is_some_and(|entry| entry.id == *id))
            {
                self.hovered = None;
            }
        }
    }

    fn update_networks(&mut self, snapshot: NetworkSnapshot) -> bool {
        let visible = matches!(self.active_tab, Tab::Overview | Tab::Network);
        if let Some(error) = snapshot.error {
            if self.network_error.as_ref() == Some(&error) {
                return false;
            }
            self.network_error = Some(error);
            return visible;
        }

        if self.network_error.is_none() && self.networks == snapshot.interfaces {
            return false;
        }

        let previous_index = self.selected_network_index().unwrap_or(0);
        let previous_selection = self.selected_network.take();
        self.networks = snapshot.interfaces;
        self.network_error = None;

        self.selected_network =
            previous_selection.filter(|name| self.networks.iter().any(|iface| iface.name == *name));

        if self.selected_network.is_none() {
            let replacement = previous_index.min(self.networks.len().saturating_sub(1));
            self.selected_network = self
                .networks
                .get(replacement)
                .map(|iface| iface.name.clone());
            self.network_detail_visible = false;
        }
        self.reconcile_hovered_network();
        self.ensure_network_visible();
        visible
    }

    fn move_network_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Network || self.networks.is_empty() {
            return false;
        }

        let current = self.selected_network_index().unwrap_or(0);
        let last = self.networks.len() - 1;
        let next = current.saturating_add_signed(delta).min(last);
        self.select_network_index(next)
    }

    pub fn select_network(&mut self, name: &str) -> bool {
        if self.active_tab != Tab::Network {
            return false;
        }
        if self.selected_network.as_deref() == Some(name) {
            return false;
        }

        if self.networks.iter().any(|iface| iface.name == name) {
            self.selected_network = Some(name.to_owned());
            self.ensure_network_visible();
            true
        } else {
            false
        }
    }

    fn select_network_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Network {
            return false;
        }
        let Some(iface) = self.networks.get(index) else {
            return false;
        };
        let name = iface.name.clone();
        if self.selected_network.as_ref() == Some(&name) {
            return false;
        }

        self.selected_network = Some(name);
        self.ensure_network_visible();
        true
    }

    fn open_network_details(&mut self) -> bool {
        if self.active_tab == Tab::Network && self.selected_network.is_some() {
            let changed = !self.network_detail_visible;
            self.network_detail_visible = true;
            self.hovered = None;
            changed
        } else {
            false
        }
    }

    fn ensure_network_visible(&mut self) {
        self.network_scroll = calculate_scroll(
            self.networks.len(),
            self.selected_network_index(),
            self.network_scroll,
            self.network_view_height,
        );
    }

    fn reconcile_hovered_network(&mut self) {
        if let Some(MouseTarget::NetworkRow(name)) = &self.hovered {
            if !self
                .networks
                .iter()
                .any(|iface| iface.name == name.as_ref())
            {
                self.hovered = None;
            }
        }
    }
}

/// Lowercased fields used for case-insensitive search and name sorting, computed
/// once per snapshot instead of once per comparison.
#[derive(Debug)]
struct ProcessKeys {
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

fn service_matches(service: &ServiceInfo, query: &str) -> bool {
    query.is_empty()
        || service.unit.to_lowercase().contains(query)
        || service.description.to_lowercase().contains(query)
        || service.load_state.to_lowercase().contains(query)
        || service.active_state.to_lowercase().contains(query)
        || service.sub_state.to_lowercase().contains(query)
}

fn log_matches(entry: &JournalEntry, query: &str) -> bool {
    query.is_empty()
        || entry.source.to_lowercase().contains(query)
        || entry.message.to_lowercase().contains(query)
        || entry.priority.is_some_and(|priority| {
            priority.to_string().contains(query)
                || entry.priority_label().contains(query)
                || (priority == 4 && "warning".contains(query))
        })
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
    use super::*;

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

    fn process(pid: u32, name: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.into(),
            cpu_percent: Some(1.0),
            memory_bytes: 1024,
            command: Some(format!("/usr/bin/{name}")),
            state: "S (sleeping)".into(),
            parent_pid: 1,
            state_code: 'S',
            start_time: u64::from(pid),
        }
    }

    fn processes(entries: Vec<ProcessInfo>) -> ProcessSnapshot {
        ProcessSnapshot {
            processes: entries,
            error: None,
        }
    }

    fn service(unit: &str, active_state: &str, description: &str) -> ServiceInfo {
        ServiceInfo {
            unit: unit.into(),
            load_state: "loaded".into(),
            active_state: active_state.into(),
            sub_state: if active_state == "active" {
                "running".into()
            } else {
                "dead".into()
            },
            description: description.into(),
        }
    }

    fn services(entries: Vec<ServiceInfo>) -> ServiceSnapshot {
        ServiceSnapshot {
            services: entries,
            error: None,
            completed_refresh_generation: 0,
        }
    }

    fn services_completed(
        entries: Vec<ServiceInfo>,
        generation: ServiceRefreshGeneration,
    ) -> ServiceSnapshot {
        ServiceSnapshot {
            completed_refresh_generation: generation,
            ..services(entries)
        }
    }

    fn visible_units(app: &App) -> Vec<&str> {
        (0..app.service_count())
            .filter_map(|index| app.service_at(index).map(|service| service.unit.as_str()))
            .collect()
    }

    fn log_entry(id: u64, source: &str, priority: u8, message: &str) -> JournalEntry {
        JournalEntry {
            id,
            timestamp_micros: Some(id.saturating_mul(1_000_000)),
            source: source.into(),
            priority: Some(priority),
            message: message.into(),
        }
    }

    fn log_batch(entries: Vec<JournalEntry>) -> JournalBatch {
        JournalBatch {
            entries,
            dropped: 0,
            error: None,
        }
    }

    fn visible_log_ids(app: &App) -> Vec<u64> {
        (0..app.log_count())
            .filter_map(|index| app.log_at(index).map(|entry| entry.id))
            .collect()
    }

    fn process_with(pid: u32, name: &str, cpu: Option<f64>, memory: u64) -> ProcessInfo {
        let mut process = process(pid, name);
        process.cpu_percent = cpu;
        process.memory_bytes = memory;
        process
    }

    fn visible_pids(app: &App) -> Vec<u32> {
        (0..app.process_count())
            .filter_map(|index| app.process_at(index).map(|process| process.pid))
            .collect()
    }

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
    fn service_filter_is_case_insensitive_across_unit_and_description() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![
            service("alpha.service", "active", "Worker"),
            service("postgresql.service", "active", "Database Server"),
        ])));

        app.update(Action::BeginServiceSearch);
        for character in "DATABASE".chars() {
            app.update(Action::AppendServiceSearch(character));
        }

        assert_eq!(visible_units(&app), vec!["postgresql.service"]);
        assert_eq!(
            app.selected_service().map(|service| service.unit.as_str()),
            Some("postgresql.service")
        );
    }

    #[test]
    fn service_navigation_keeps_selection_visible() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(
            (0..6)
                .map(|index| service(&format!("service-{index}.service"), "active", ""))
                .collect(),
        )));
        app.update(Action::ServiceViewportChanged {
            start: 0,
            height: 2,
        });

        app.update(Action::ServiceNextPage);
        assert_eq!(app.selected_service_index(), Some(2));
        assert_eq!(app.service_scroll(), 1);

        app.update(Action::ServiceLast);
        assert_eq!(app.selected_service_index(), Some(5));
        assert_eq!(app.service_scroll(), 4);
    }

    #[test]
    fn service_refresh_preserves_selection_by_unit_name() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![
            service("alpha.service", "inactive", "Alpha"),
            service("beta.service", "active", "Beta"),
        ])));
        app.update(Action::SelectService("beta.service".into()));

        app.update(Action::ServicesUpdated(services(vec![
            service("beta.service", "inactive", "Beta changed"),
            service("alpha.service", "active", "Alpha"),
        ])));

        assert_eq!(
            app.selected_service().map(|service| service.unit.as_str()),
            Some("beta.service")
        );
    }

    #[test]
    fn disappearing_service_is_nonfatal_and_closes_details() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![
            service("alpha.service", "active", "Alpha"),
            service("beta.service", "active", "Beta"),
        ])));
        app.update(Action::SelectService("beta.service".into()));
        app.update(Action::OpenServiceDetails);

        app.update(Action::ServicesUpdated(services(vec![service(
            "alpha.service",
            "inactive",
            "Alpha",
        )])));

        assert_eq!(
            app.selected_service().map(|service| service.unit.as_str()),
            Some("alpha.service")
        );
        assert!(!app.service_detail_visible());
    }

    #[test]
    fn service_refresh_is_an_explicit_background_request() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));

        assert!(app.update(Action::RefreshServices));
        assert!(app.service_refreshing());
        assert_eq!(app.take_service_refresh_request(), Some(1));
        assert_eq!(app.take_service_refresh_request(), None);

        app.update(Action::ServicesUpdated(services(vec![service(
            "alpha.service",
            "active",
            "Alpha",
        )])));
        assert!(app.service_refreshing());

        assert!(app.update(Action::ServicesUpdated(services_completed(
            vec![service("alpha.service", "active", "Alpha")],
            1,
        ))));
        assert!(!app.service_refreshing());
    }

    #[test]
    fn service_refresh_waits_for_the_latest_coalesced_generation() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));

        assert!(app.update(Action::RefreshServices));
        assert!(!app.update(Action::RefreshServices));
        assert!(!app.update(Action::RefreshServices));
        assert_eq!(app.take_service_refresh_request(), Some(3));

        app.update(Action::ServicesUpdated(services_completed(Vec::new(), 2)));
        assert!(app.service_refreshing());

        assert!(app.update(Action::ServicesUpdated(services_completed(Vec::new(), 3,))));
        assert!(!app.service_refreshing());
    }

    #[test]
    fn service_collection_failure_preserves_the_last_good_snapshot() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![service(
            "alpha.service",
            "active",
            "Alpha",
        )])));

        app.update(Action::ServicesUpdated(ServiceSnapshot {
            services: Vec::new(),
            error: Some("system bus unavailable".into()),
            completed_refresh_generation: 0,
        }));

        assert_eq!(visible_units(&app), vec!["alpha.service"]);
        assert_eq!(app.service_error(), Some("system bus unavailable"));
    }

    #[test]
    fn logs_follow_latest_entry_until_manual_navigation() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![
            log_entry(1, "kernel", 6, "one"),
            log_entry(2, "sshd.service", 4, "two"),
        ])));

        assert_eq!(app.selected_log().map(|entry| entry.id), Some(2));
        assert!(app.log_following());

        app.update(Action::LogPrevious);
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(1));
        assert!(!app.log_following());

        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            3,
            "dbus.service",
            6,
            "three",
        )])));
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(1));
    }

    #[test]
    fn pausing_preserves_position_and_resuming_catches_up_when_following() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            1, "kernel", 6, "one",
        )])));
        app.update(Action::ToggleLogPause);

        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            2, "kernel", 6, "two",
        )])));
        assert!(app.log_paused());
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(1));

        app.update(Action::ToggleLogPause);
        assert!(!app.log_paused());
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(2));
    }

    #[test]
    fn log_search_is_case_insensitive() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![
            log_entry(1, "kernel", 6, "device ready"),
            log_entry(2, "SSHD.service", 4, "Login Failed"),
        ])));
        app.update(Action::BeginLogSearch);
        for character in "login FAILED".chars() {
            app.update(Action::AppendLogSearch(character));
        }

        assert_eq!(visible_log_ids(&app), vec![2]);
    }

    #[test]
    fn log_buffer_evicts_oldest_entries_at_capacity() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        let entries = (1..=LOG_BUFFER_CAPACITY as u64 + 1)
            .map(|id| log_entry(id, "test", 6, "entry"))
            .collect();

        app.update(Action::LogsUpdated(log_batch(entries)));

        assert_eq!(app.log_count(), LOG_BUFFER_CAPACITY);
        assert_eq!(app.log_at(0).map(|entry| entry.id), Some(2));
        assert_eq!(
            app.log_at(LOG_BUFFER_CAPACITY - 1).map(|entry| entry.id),
            Some(LOG_BUFFER_CAPACITY as u64 + 1)
        );
    }

    #[test]
    fn log_drop_and_error_metadata_is_nonfatal() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(JournalBatch {
            entries: vec![log_entry(1, "kernel", 3, "failure")],
            dropped: 7,
            error: Some("journal stream ended".into()),
        }));

        assert_eq!(app.log_count(), 1);
        assert_eq!(app.log_dropped(), 7);
        assert_eq!(app.log_error(), Some("journal stream ended"));
    }

    #[test]
    fn journal_ingestion_does_not_redraw_an_inactive_screen() {
        let mut app = App::default();

        assert!(!app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            1,
            "kernel",
            6,
            "background entry",
        )]))));
        assert_eq!(app.log_count(), 1);
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
    fn hover_only_invalidates_rendering_on_semantic_transitions() {
        let mut app = App::default();
        let target = MouseTarget::Tab(Tab::Processes);

        assert!(app.update(Action::HoverMouseTarget(Some(target.clone()))));
        assert!(!app.update(Action::HoverMouseTarget(Some(target))));
        assert!(app.update(Action::HoverMouseTarget(None)));
        assert!(!app.update(Action::HoverMouseTarget(None)));
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
    fn scroll_calculation_handles_resize_and_empty_viewports() {
        assert_eq!(calculate_scroll(20, Some(10), 0, 5), 6);
        assert_eq!(calculate_scroll(20, Some(10), 6, 2), 9);
        assert_eq!(calculate_scroll(3, Some(2), usize::MAX, 20), 0);
        assert_eq!(calculate_scroll(20, Some(10), 0, 0), 10);
        assert_eq!(calculate_scroll(0, None, usize::MAX, 0), 0);
    }

    fn dummy_network(name: &str) -> NetworkInterfaceInfo {
        NetworkInterfaceInfo {
            name: name.to_string(),
            operstate: crate::linux::OperState::Up,
            mac_address: Some("00:11:22:33:44:55".to_string()),
            mtu: Some(1500),
            ipv4_addresses: Vec::new(),
            ipv6_addresses: Vec::new(),
            rx_bytes: 1000,
            tx_bytes: 1000,
            rx_packets: 10,
            tx_packets: 10,
            rx_errors: 0,
            tx_errors: 0,
            rx_dropped: 0,
            tx_dropped: 0,
            rx_rate_bytes_per_sec: Some(100.0),
            tx_rate_bytes_per_sec: Some(100.0),
        }
    }

    #[test]
    fn network_selection_is_preserved_by_name() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0"), dummy_network("wlan0")],
            error: None,
        }));
        app.update(Action::SelectNetwork("wlan0".into()));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );

        // Update with reordered list
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![
                dummy_network("docker0"),
                dummy_network("eth0"),
                dummy_network("wlan0"),
            ],
            error: None,
        }));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );
    }

    #[test]
    fn disappearing_network_interface_recovers_and_closes_details() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![
                dummy_network("eth0"),
                dummy_network("wlan0"),
                dummy_network("veth1"),
            ],
            error: None,
        }));
        app.update(Action::SelectNetwork("wlan0".into()));
        app.update(Action::OpenNetworkDetails);
        assert!(app.network_detail_visible());

        // wlan0 disappears
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0"), dummy_network("veth1")],
            error: None,
        }));

        // Selection clamped to nearest and details closed
        assert!(!app.network_detail_visible());
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("veth1")
        );
    }

    #[test]
    fn network_keyboard_navigation() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![
                dummy_network("eth0"),
                dummy_network("wlan0"),
                dummy_network("veth1"),
            ],
            error: None,
        }));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("eth0")
        );

        app.update(Action::NetworkNext);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );

        app.update(Action::NetworkLast);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("veth1")
        );

        app.update(Action::NetworkPrevious);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );

        app.update(Action::NetworkFirst);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("eth0")
        );
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

    const SAMPLING: Duration = Duration::from_secs(1);
    const SERVICES_PERIOD: Duration = Duration::from_secs(5);

    fn stale_names(app: &App) -> Vec<&'static str> {
        app.stale_collectors().map(Collector::label).collect()
    }

    fn fresh_tick(app: &mut App, now: Instant) -> bool {
        app.update(Action::SystemMetricsUpdated(SystemMetrics::default()));
        app.update(Action::ProcessesUpdated(ProcessSnapshot::default()));
        app.update(Action::NetworkUpdated(NetworkSnapshot::default()));
        app.update(Action::ServicesUpdated(ServiceSnapshot::default()));
        app.update(Action::Tick(now))
    }

    #[test]
    fn staleness_is_disabled_without_configured_periods() {
        let mut app = App::default();
        let start = Instant::now();

        app.update(Action::Tick(start));
        assert!(!app.update(Action::Tick(start + Duration::from_secs(3600))));
        assert!(stale_names(&app).is_empty());
    }

    #[test]
    fn collector_becomes_stale_only_after_three_missed_periods() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
        let start = Instant::now();
        fresh_tick(&mut app, start);

        assert!(!app.update(Action::Tick(start + SAMPLING * 3)));
        assert!(
            stale_names(&app).is_empty(),
            "exactly at the threshold is fresh"
        );

        assert!(app.update(Action::Tick(
            start + SAMPLING * 3 + Duration::from_millis(1)
        )));
        assert_eq!(stale_names(&app), ["metrics", "processes", "network"]);
    }

    #[test]
    fn stale_transitions_redraw_once_and_recovery_clears_the_marker() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
        let start = Instant::now();
        fresh_tick(&mut app, start);

        assert!(app.update(Action::Tick(start + Duration::from_secs(4))));
        for seconds in 5..10 {
            assert!(
                !app.update(Action::Tick(start + Duration::from_secs(seconds))),
                "no redraw while staying stale"
            );
        }

        app.update(Action::ProcessesUpdated(processes(vec![process(
            1, "back",
        )])));
        assert!(app.update(Action::Tick(start + Duration::from_secs(10))));
        assert_eq!(stale_names(&app), ["metrics", "network"]);
    }

    #[test]
    fn hidden_collector_transitions_do_not_redraw() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
        app.update(Action::SelectTab(Tab::Logs));
        let start = Instant::now();
        fresh_tick(&mut app, start);

        assert!(!app.update(Action::Tick(start + Duration::from_secs(60))));
        assert!(stale_names(&app).is_empty());

        app.update(Action::SelectTab(Tab::Services));
        assert_eq!(stale_names(&app), ["services"]);
    }

    #[test]
    fn services_threshold_exceeds_the_systemctl_timeout() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
        app.update(Action::SelectTab(Tab::Services));
        let start = Instant::now();
        fresh_tick(&mut app, start);
        let slowest_healthy_gap = SERVICES_PERIOD + SYSTEMCTL_TIMEOUT;

        assert!(!app.update(Action::Tick(start + slowest_healthy_gap)));
        assert!(stale_names(&app).is_empty());
        assert!(app.update(Action::Tick(
            start + SERVICES_PERIOD * 2 + SYSTEMCTL_TIMEOUT + Duration::from_millis(1)
        )));
        assert_eq!(stale_names(&app), ["services"]);
    }

    #[test]
    fn collector_that_never_publishes_becomes_stale_from_the_first_tick() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
        let start = Instant::now();

        assert!(!app.update(Action::Tick(start)));
        assert!(
            stale_names(&app).is_empty(),
            "starting collectors are not stale"
        );
        assert!(app.update(Action::Tick(start + Duration::from_secs(4))));
        assert_eq!(stale_names(&app), ["metrics", "processes", "network"]);
    }

    #[test]
    fn staleness_is_checked_while_a_modal_is_open() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
        let start = Instant::now();
        fresh_tick(&mut app, start);
        app.update(Action::ShowHelp);

        assert!(app.update(Action::Tick(start + Duration::from_secs(4))));
        assert!(!stale_names(&app).is_empty());
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

    #[test]
    fn network_selection_and_details_guarded_by_active_tab() {
        let mut app = App::default();
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0"), dummy_network("eth1")],
            error: None,
        }));

        // On Overview tab, network selection actions should return false
        assert_eq!(app.active_tab(), Tab::Overview);
        assert!(!app.update(Action::SelectNetwork("eth1".into())));
        assert!(!app.update(Action::NetworkFirst));
        assert!(!app.update(Action::NetworkNext));

        // Switch to Network tab
        app.update(Action::SelectTab(Tab::Network));
        assert!(app.update(Action::SelectNetwork("eth1".into())));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("eth1")
        );

        // Open network details should open, not toggle
        assert!(app.update(Action::OpenNetworkDetails));
        assert!(app.network_detail_visible());
        assert!(!app.update(Action::OpenNetworkDetails));
        assert!(app.network_detail_visible());
    }

    #[test]
    fn log_matches_matches_priority_names_and_numbers() {
        let error_entry = JournalEntry {
            id: 1,
            timestamp_micros: None,
            source: "app".into(),
            priority: Some(3),
            message: "something happened".into(),
        };
        let warn_entry = JournalEntry {
            id: 2,
            timestamp_micros: None,
            source: "app".into(),
            priority: Some(4),
            message: "look out".into(),
        };

        assert!(log_matches(&error_entry, "error"));
        assert!(log_matches(&error_entry, "err"));
        assert!(log_matches(&error_entry, "3"));
        assert!(!log_matches(&error_entry, "warn"));

        assert!(log_matches(&warn_entry, "warn"));
        assert!(log_matches(&warn_entry, "warning"));
        assert!(log_matches(&warn_entry, "4"));
        assert!(!log_matches(&warn_entry, "crit"));
    }
}
