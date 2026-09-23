use std::{sync::Arc, time::Instant};

use crate::linux::{
    HardwareInventory, JournalBatch, NetworkSnapshot, ProcessIdentity, ProcessSignal,
    ProcessSnapshot, ServiceSnapshot, SystemMetrics,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,
    SelectTab(Tab),
    NextTab,
    PreviousTab,
    ShowHelp,
    Escape,
    StepSamplingInterval(IntervalStep),
    SystemMetricsUpdated(SystemMetrics),
    HardwareDiscovered(HardwareInventory),
    ProcessesUpdated(ProcessSnapshot),
    ProcessPrevious,
    ProcessNext,
    ProcessPreviousPage,
    ProcessNextPage,
    ProcessFirst,
    ProcessLast,
    SelectProcess(ProcessIdentity),
    BeginProcessSearch,
    AppendProcessSearch(char),
    BackspaceProcessSearch,
    OpenProcessDetails,
    RequestProcessSignal(ProcessSignal),
    CancelProcessSignal,
    ConfirmProcessSignal,
    ToggleProcessSignalFocus,
    FocusProcessSignal(SignalConfirmButton),
    ExecuteFocusedProcessSignal,
    SortProcesses(ProcessSortField),
    /// Pin or unpin the selected process.
    TogglePin,
    MoveSelectedPin(PinMove),
    ServicesUpdated(ServiceSnapshot),
    ServicePrevious,
    ServiceNext,
    ServicePreviousPage,
    ServiceNextPage,
    ServiceFirst,
    ServiceLast,
    SelectService(Arc<str>),
    BeginServiceSearch,
    AppendServiceSearch(char),
    BackspaceServiceSearch,
    OpenServiceDetails,
    RefreshServices,
    LogsUpdated(JournalBatch),
    LogPrevious,
    LogNext,
    LogPreviousPage,
    LogNextPage,
    LogFirst,
    LogLast,
    SelectLog(u64),
    BeginLogSearch,
    AppendLogSearch(char),
    BackspaceLogSearch,
    OpenLogDetails,
    ToggleLogFollow,
    ToggleLogPause,
    NetworkUpdated(NetworkSnapshot),
    NetworkPrevious,
    NetworkNext,
    NetworkPreviousPage,
    NetworkNextPage,
    NetworkFirst,
    NetworkLast,
    SelectNetwork(Arc<str>),
    OpenNetworkDetails,
    HoverMouseTarget(Option<MouseTarget>),
    ProcessViewportChanged {
        start: usize,
        height: usize,
    },
    ServiceViewportChanged {
        start: usize,
        height: usize,
    },
    LogViewportChanged {
        start: usize,
        height: usize,
    },
    NetworkViewportChanged {
        start: usize,
        height: usize,
    },
    Resize,
    Tick(Instant),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseTarget {
    Tab(Tab),
    ProcessRow(ProcessIdentity),
    ProcessSortHeader(ProcessSortField),
    ProcessSignalCancel,
    ProcessSignalConfirm,
    ServiceRow(Arc<str>),
    LogRow(u64),
    NetworkRow(Arc<str>),
}

/// Direction in which a pinned process moves within the pinned section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMove {
    Up,
    Down,
}

/// Direction of a `+`/`-` step through the sampling presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalStep {
    Longer,
    Shorter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignalConfirmButton {
    #[default]
    Cancel,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortField {
    Cpu,
    Memory,
    Pid,
    Name,
}

impl ProcessSortField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "MEM",
            Self::Pid => "PID",
            Self::Name => "NAME",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSort {
    pub field: ProcessSortField,
    pub descending: bool,
}

impl Default for ProcessSort {
    fn default() -> Self {
        Self {
            field: ProcessSortField::Cpu,
            descending: true,
        }
    }
}

impl ProcessSort {
    pub const fn for_field(field: ProcessSortField) -> Self {
        Self {
            field,
            descending: matches!(field, ProcessSortField::Cpu | ProcessSortField::Memory),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    Normal,
    ProcessSearch,
    ProcessDetail,
    ProcessSignalConfirm,
    Services,
    ServiceSearch,
    ServiceDetail,
    Logs,
    LogSearch,
    LogDetail,
    Network,
    NetworkDetail,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Processes,
    Services,
    Logs,
    Network,
}

impl Tab {
    pub const ALL: [Self; 5] = [
        Self::Overview,
        Self::Processes,
        Self::Services,
        Self::Logs,
        Self::Network,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Processes => "Processes",
            Self::Services => "Services",
            Self::Logs => "Logs",
            Self::Network => "Network",
        }
    }

    /// Compact label used when the full labels cannot fit with separation.
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Overview => "Ovr",
            Self::Processes => "Proc",
            Self::Services => "Svc",
            Self::Logs => "Logs",
            Self::Network => "Net",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Overview => Self::Processes,
            Self::Processes => Self::Services,
            Self::Services => Self::Logs,
            Self::Logs => Self::Network,
            Self::Network => Self::Overview,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Overview => Self::Network,
            Self::Processes => Self::Overview,
            Self::Services => Self::Processes,
            Self::Logs => Self::Services,
            Self::Network => Self::Logs,
        }
    }
}
