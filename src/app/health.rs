//! Collector staleness tracking behind the `stale` marker.

use super::*;

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

        app.update(Action::SelectTab(Tab::Processes));
        assert_eq!(stale_names(&app), ["metrics", "processes"]);
    }

    #[test]
    fn paused_services_are_not_stale_when_their_tab_returns() {
        let mut app = App::default().with_collector_periods(SAMPLING, SERVICES_PERIOD);
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
}
