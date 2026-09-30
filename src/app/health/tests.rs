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
    let mut app = App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(
        Duration::from_millis(250),
    ));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
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
    let mut app = App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(
        Duration::from_secs(60),
    ));
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
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
    for sample in 0..10 {
        app.update(Action::SystemMetricsUpdated(SystemMetrics {
            cpu_percent: Some(f64::from(sample)),
            memory: Some(crate::linux::ByteUsage { used: 1, total: 4 }),
            ..SystemMetrics::default()
        }));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0")],
            error: None,
        }));
    }
    assert_eq!(app.aggregate_cpu_history().iter().count(), 10);
    assert_eq!(app.memory_history().iter().count(), 10);
    assert_eq!(app.network_history().iter().count(), 10);

    step(&mut app, IntervalStep::Longer);

    assert_eq!(app.aggregate_cpu_history().iter().count(), 0);
    assert_eq!(app.memory_history().iter().count(), 0);
    assert_eq!(app.network_history().iter().count(), 0);
    assert_eq!(app.cpu_history_interval(), Duration::from_secs(2));
}

#[test]
fn interval_keys_are_ignored_while_an_overlay_is_open() {
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
    app.update(Action::ShowHelp);

    assert!(!step(&mut app, IntervalStep::Longer));
    assert_eq!(app.sampling_interval_label(), "1s");
}

#[test]
fn staleness_is_checked_while_a_modal_is_open() {
    let mut app =
        App::default().with_collector_periods(CollectorPeriods::for_sampling_interval(SAMPLING));
    let start = Instant::now();
    fresh_tick(&mut app, start);
    app.update(Action::ShowHelp);

    assert!(app.update(Action::Tick(start + Duration::from_secs(4))));
    assert!(!stale_names(&app).is_empty());
}
