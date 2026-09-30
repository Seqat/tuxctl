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
pub(crate) use network::overview_interfaces;
use processes::{PinnedProcess, ProcessKeys, ProcessRow, ProcessView};
use services::ServiceView;

const LOG_BUFFER_CAPACITY: usize = 2_000;
/// Samples kept per history: enough for a graph as wide as a card on a very
/// wide terminal (a fixed 1.9 KiB each).
const METRIC_HISTORY_CAPACITY: usize = 240;

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

    #[cfg(test)]
    pub(crate) fn push_for_test(&mut self, value: f64) {
        self.push(value);
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
    /// Whether NVML may be loaded for NVIDIA GPUs, shown in Help.
    nvidia_temperature: bool,
    aggregate_cpu_history: MetricHistory,
    memory_history: MetricHistory,
    /// RX+TX bytes/s of the interface the Overview's Network card is about.
    network_history: MetricHistory,
    /// That interface; its history starts over when it changes.
    network_history_interface: Option<String>,
    /// Utilization of the GPU the Overview's GPU card is about.
    gpu_history: MetricHistory,
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
            nvidia_temperature: false,
            aggregate_cpu_history: MetricHistory::default(),
            memory_history: MetricHistory::default(),
            network_history: MetricHistory::default(),
            network_history_interface: None,
            gpu_history: MetricHistory::default(),
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
    /// Whether the hardware sensors are shown (only the Overview shows them).
    pub fn sensors_visible(&self) -> bool {
        self.active_tab == Tab::Overview
    }

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

    /// Whether a popup (menu, dialog or details) covers the screen.
    pub fn overlay_open(&self) -> bool {
        self.overlay.is_some()
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

    pub fn gpu_history(&self) -> &MetricHistory {
        &self.gpu_history
    }

    pub fn hardware(&self) -> Option<&HardwareInventory> {
        self.hardware.as_ref()
    }

    pub fn with_nvidia_temperature(mut self, enabled: bool) -> Self {
        self.nvidia_temperature = enabled;
        self
    }

    pub fn nvidia_temperature(&self) -> bool {
        self.nvidia_temperature
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
                let gpu_recorded = self
                    .hardware
                    .as_ref()
                    .and_then(|inventory| inventory.primary_gpu()?.device_path.clone())
                    .and_then(|path| metrics.gpus.iter().find(|gpu| gpu.device_path == path))
                    .and_then(|gpu| gpu.utilization)
                    .is_some_and(|utilization| self.gpu_history.push_percent(utilization));
                let history_changed = cpu_recorded || memory_recorded || gpu_recorded;
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
            Action::RequestQuit => self.request_quit(),
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
            // Handled before this match, so no modal can suppress it.
            Action::Resize => true,
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
        // The GPU is not sampled while the Overview is hidden; a graph spanning
        // that gap would misstate its time axis.
        if tab == Tab::Overview && self.active_tab != Tab::Overview {
            self.gpu_history.clear();
        }
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

    /// `q` never quits by itself: it opens the main menu on Exit, where Enter
    /// or `q` confirms. A pending signal confirmation is cancelled first.
    fn request_quit(&mut self) -> bool {
        self.cancel_process_signal();
        let menu = Some(Overlay::Menu {
            selected: MenuItem::Exit,
        });
        if self.overlay == menu {
            return false;
        }
        self.overlay = menu;
        self.hovered = None;
        true
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
                // The menu is the confirmation step (also for `q`).
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
mod tests;
