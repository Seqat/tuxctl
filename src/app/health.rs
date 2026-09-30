//! Collector staleness tracking behind the `stale` marker.

use super::*;

/// Selectable sampling intervals, for `--interval` and the `+`/`-` keys.
/// Shorter intervals are out of scope: the main loop picks up snapshots on a
/// 250 ms tick and /proc sampling gets noisy.
pub const SAMPLING_PRESETS: [(&str, Duration); 8] = [
    ("250ms", Duration::from_millis(250)),
    ("500ms", Duration::from_millis(500)),
    ("1s", Duration::from_secs(1)),
    ("2s", Duration::from_secs(2)),
    ("5s", Duration::from_secs(5)),
    ("10s", Duration::from_secs(10)),
    ("30s", Duration::from_secs(30)),
    ("60s", Duration::from_secs(60)),
];
pub const DEFAULT_SAMPLING_INTERVAL: Duration = Duration::from_secs(1);

/// The preset next to `current` in the direction of `step`, if any.
fn step_preset(current: Duration, step: IntervalStep) -> Option<Duration> {
    let index = SAMPLING_PRESETS
        .iter()
        .position(|(_, interval)| *interval == current)?;
    let next = match step {
        IntervalStep::Longer => index.checked_add(1)?,
        IntervalStep::Shorter => index.checked_sub(1)?,
    };
    SAMPLING_PRESETS.get(next).map(|(_, interval)| *interval)
}

/// How often each background collector samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectorPeriods {
    pub metrics: Duration,
    pub processes: Duration,
    pub network: Duration,
    pub services: Duration,
}

impl CollectorPeriods {
    /// Full process scans stay at most once per second and `systemctl` listings
    /// at most once every 5 s, whatever the sampling interval.
    const MIN_PROCESS_PERIOD: Duration = Duration::from_secs(1);
    const MIN_SERVICE_PERIOD: Duration = Duration::from_secs(5);

    pub fn for_sampling_interval(interval: Duration) -> Self {
        Self {
            metrics: interval,
            processes: interval.max(Self::MIN_PROCESS_PERIOD),
            network: interval,
            services: interval.max(Self::MIN_SERVICE_PERIOD),
        }
    }
}

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
pub(super) struct CollectorHealth {
    /// Disabled until [`App::with_collector_periods`] configures a threshold.
    stale_after: Option<Duration>,
    /// Tick time at which data last arrived; the first tick is the baseline so a
    /// collector that never publishes still becomes stale.
    last_update: Option<Instant>,
    updated_since_tick: bool,
    stale: bool,
}

impl App {
    /// Enables stale markers: a collector is stale after three missed periods.
    /// Services also allow for a full `systemctl` timeout before being flagged.
    pub fn with_collector_periods(mut self, periods: CollectorPeriods) -> Self {
        self.apply_collector_periods(periods);
        self
    }

    /// Label of the current sampling interval, e.g. `1s`.
    pub fn sampling_interval_label(&self) -> &'static str {
        SAMPLING_PRESETS
            .iter()
            .find(|(_, interval)| *interval == self.sampling_interval)
            .map_or("?", |(label, _)| label)
    }

    /// New collector periods after the interval changed, for the main loop to
    /// hand to the running collectors.
    pub fn take_sampling_interval_change(&mut self) -> Option<CollectorPeriods> {
        std::mem::take(&mut self.sampling_interval_changed)
            .then(|| CollectorPeriods::for_sampling_interval(self.sampling_interval))
    }

    /// Moves to the neighbouring preset; `false` at either end.
    pub(super) fn step_sampling_interval(&mut self, step: IntervalStep) -> bool {
        let Some(interval) = step_preset(self.sampling_interval, step) else {
            return false;
        };
        let staleness_enabled = self
            .collector_health
            .iter()
            .any(|health| health.stale_after.is_some());
        if staleness_enabled {
            self.apply_collector_periods(CollectorPeriods::for_sampling_interval(interval));
            // Collectors pick up the new period on their next wake; judge them
            // against it from the next tick instead of flagging the old gap.
            for health in &mut self.collector_health {
                health.last_update = None;
                health.updated_since_tick = false;
                health.stale = false;
            }
        } else {
            self.sampling_interval = interval;
            self.cpu_history_interval = interval;
        }
        // Samples taken at another interval would make the window label wrong.
        self.aggregate_cpu_history.clear();
        self.memory_history.clear();
        self.network_history.clear();
        self.gpu_history.clear();
        self.sampling_interval_changed = true;
        true
    }

    fn apply_collector_periods(&mut self, periods: CollectorPeriods) {
        for collector in Collector::ALL {
            let threshold = match collector {
                Collector::Metrics => periods.metrics.saturating_mul(3),
                Collector::Processes => periods.processes.saturating_mul(3),
                Collector::Network => periods.network.saturating_mul(3),
                Collector::Services => periods.services.saturating_mul(2) + SYSTEMCTL_TIMEOUT,
            };
            self.collector_health[collector.index()].stale_after = Some(threshold);
        }
        self.sampling_interval = periods.metrics;
        self.cpu_history_interval = periods.metrics;
    }

    /// Time between aggregate CPU history samples.
    pub fn cpu_history_interval(&self) -> Duration {
        self.cpu_history_interval
    }

    /// Stale collectors whose data is shown on the active tab.
    pub fn stale_collectors(&self) -> impl Iterator<Item = Collector> + '_ {
        Collector::ALL.into_iter().filter(|collector| {
            self.collector_health[collector.index()].stale && collector.shown_on(self.active_tab)
        })
    }

    pub(super) fn record_collector_update(&mut self, collector: Collector) {
        self.collector_health[collector.index()].updated_since_tick = true;
    }

    /// Returns `true` only when a collector shown on the active tab changes
    /// between fresh and stale.
    pub(super) fn check_collector_staleness(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        for collector in Collector::ALL {
            let health = &mut self.collector_health[collector.index()];
            let Some(stale_after) = health.stale_after else {
                continue;
            };
            if collector == Collector::Services && !collector.shown_on(self.active_tab) {
                // Paused while hidden: keep the clock fresh so re-entering the
                // tab does not count the pause as missed updates.
                health.last_update = Some(now);
                health.updated_since_tick = false;
                health.stale = false;
                continue;
            }
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
}

#[cfg(test)]
mod tests;
