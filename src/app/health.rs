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
mod tests {
    use super::super::test_support::*;
    use super::*;

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
    fn sampling_interval_is_clamped_for_process_and_service_collectors() {
        let fast = CollectorPeriods::for_sampling_interval(Duration::from_millis(250));
        assert_eq!(fast.metrics, Duration::from_millis(250));
        assert_eq!(fast.network, Duration::from_millis(250));
        assert_eq!(fast.processes, Duration::from_secs(1));
        assert_eq!(fast.services, Duration::from_secs(5));

        let slow = CollectorPeriods::for_sampling_interval(Duration::from_secs(30));
        assert_eq!(slow.processes, Duration::from_secs(30));
        assert_eq!(slow.services, Duration::from_secs(30));
    }

    #[test]
    fn each_collector_uses_its_own_period_for_staleness() {
        let mut app = App::default().with_collector_periods(
            CollectorPeriods::for_sampling_interval(Duration::from_millis(250)),
        );
        app.update(Action::SelectTab(Tab::Processes));
        let start = Instant::now();
        fresh_tick(&mut app, start);

        // 1 s later: metrics (250 ms period) are stale, the 1 s process scan is not.
        app.update(Action::Tick(start + Duration::from_secs(1)));
        assert_eq!(stale_names(&app), ["metrics"]);

        app.update(Action::Tick(start + Duration::from_millis(3_001)));
        assert_eq!(stale_names(&app), ["metrics", "processes"]);
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
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        app.update(Action::SelectTab(Tab::Logs));
        let start = Instant::now();
        fresh_tick(&mut app, start);

        assert!(!app.update(Action::Tick(start + Duration::from_secs(60))));
        assert!(stale_names(&app).is_empty());

        app.update(Action::SelectTab(Tab::Processes));
        assert_eq!(stale_names(&app), ["metrics", "processes"]);
    }

    #[test]
    fn paused_services_are_not_stale_when_their_tab_returns() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        app.update(Action::SelectTab(Tab::Services));
        let start = Instant::now();
        fresh_tick(&mut app, start);
        app.update(Action::SelectTab(Tab::Overview));

        let back = start + Duration::from_secs(600);
        app.update(Action::Tick(back));
        app.update(Action::SelectTab(Tab::Services));

        assert!(!app.update(Action::Tick(back + Duration::from_millis(250))));
        assert!(
            stale_names(&app).is_empty(),
            "the pause is not a missed update"
        );
        let threshold = SERVICES_PERIOD * 2 + SYSTEMCTL_TIMEOUT;
        assert!(app.update(Action::Tick(back + threshold + Duration::from_secs(1))));
        assert_eq!(stale_names(&app), ["services"]);
    }

    #[test]
    fn services_threshold_exceeds_the_systemctl_timeout() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        let start = Instant::now();

        assert!(!app.update(Action::Tick(start)));
        assert!(
            stale_names(&app).is_empty(),
            "starting collectors are not stale"
        );
        assert!(app.update(Action::Tick(start + Duration::from_secs(4))));
        assert_eq!(stale_names(&app), ["metrics", "processes", "network"]);
    }

    fn step(app: &mut App, step: IntervalStep) -> bool {
        app.update(Action::StepSamplingInterval(step))
    }

    #[test]
    fn interval_steps_walk_the_presets_and_stop_at_both_ends() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        assert_eq!(app.sampling_interval_label(), "1s");

        let mut labels = Vec::new();
        while step(&mut app, IntervalStep::Longer) {
            labels.push(app.sampling_interval_label());
        }
        assert_eq!(labels, ["2s", "5s", "10s", "30s", "60s"]);
        assert!(app.take_sampling_interval_change().is_some());
        assert!(
            !step(&mut app, IntervalStep::Longer),
            "no redraw at the end"
        );
        assert!(app.take_sampling_interval_change().is_none());

        while step(&mut app, IntervalStep::Shorter) {}
        assert_eq!(app.sampling_interval_label(), "250ms");
        assert!(!step(&mut app, IntervalStep::Shorter));
    }

    #[test]
    fn interval_change_hands_clamped_periods_to_the_collectors_once() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        assert!(app.take_sampling_interval_change().is_none());

        assert!(step(&mut app, IntervalStep::Shorter));
        assert!(step(&mut app, IntervalStep::Shorter));

        let periods = app.take_sampling_interval_change().expect("changed");
        assert_eq!(periods.metrics, Duration::from_millis(250));
        assert_eq!(periods.network, Duration::from_millis(250));
        assert_eq!(periods.processes, Duration::from_secs(1));
        assert_eq!(periods.services, Duration::from_secs(5));
        assert!(app.take_sampling_interval_change().is_none(), "taken once");
    }

    #[test]
    fn interval_change_applies_new_thresholds_without_a_false_stale_flash() {
        let mut app = App::default().with_collector_periods(
            CollectorPeriods::for_sampling_interval(Duration::from_secs(60)),
        );
        let start = Instant::now();
        fresh_tick(&mut app, start);

        // 10 s after the last 60 s sample, switch to 250 ms (threshold 750 ms).
        let changed = start + Duration::from_secs(10);
        assert!(!app.update(Action::Tick(changed)));
        while step(&mut app, IntervalStep::Shorter) {}
        assert_eq!(app.sampling_interval_label(), "250ms");

        assert!(!app.update(Action::Tick(changed + Duration::from_millis(250))));
        assert!(stale_names(&app).is_empty(), "the old gap is not a miss");

        assert!(app.update(Action::Tick(changed + Duration::from_millis(1_100))));
        assert_eq!(stale_names(&app), ["metrics", "network"]);
    }

    #[test]
    fn interval_change_clears_the_cpu_history_and_updates_its_window() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        for sample in 0..10 {
            app.update(Action::SystemMetricsUpdated(SystemMetrics {
                cpu_percent: Some(f64::from(sample)),
                ..SystemMetrics::default()
            }));
        }
        assert_eq!(app.aggregate_cpu_history().iter().count(), 10);

        step(&mut app, IntervalStep::Longer);

        assert_eq!(app.aggregate_cpu_history().iter().count(), 0);
        assert_eq!(app.cpu_history_interval(), Duration::from_secs(2));
    }

    #[test]
    fn interval_keys_are_ignored_while_an_overlay_is_open() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        app.update(Action::ShowHelp);

        assert!(!step(&mut app, IntervalStep::Longer));
        assert_eq!(app.sampling_interval_label(), "1s");
    }

    #[test]
    fn staleness_is_checked_while_a_modal_is_open() {
        let mut app = App::default()
            .with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
        let start = Instant::now();
        fresh_tick(&mut app, start);
        app.update(Action::ShowHelp);

        assert!(app.update(Action::Tick(start + Duration::from_secs(4))));
        assert!(!stale_names(&app).is_empty());
    }
}
