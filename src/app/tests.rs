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
fn request_quit_opens_the_menu_on_exit_from_every_overlay() {
    let mut app = App::default();
    assert!(app.update(Action::RequestQuit));
    assert!(!app.should_quit());
    assert_eq!(app.menu_selection(), Some(MenuItem::Exit));
    assert!(!app.update(Action::RequestQuit), "already open");
    app.update(Action::ActivateSelectedMenuItem);
    assert!(app.should_quit());

    for kind in OVERLAYS {
        let mut app = app_with_overlay(kind);
        app.update(Action::RequestQuit);
        assert!(!app.should_quit(), "{kind:?}");
        assert_eq!(app.menu_selection(), Some(MenuItem::Exit), "{kind:?}");
        assert!(app.process_signal_confirmation().is_none(), "{kind:?}");
        assert!(app.update(Action::Escape), "{kind:?} Esc cancels");
        assert!(app.overlay.is_none(), "{kind:?}");
        assert!(!app.should_quit(), "{kind:?}");
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
fn gpu_mount_and_swap_changes_redraw_only_the_overview() {
    use std::{path::Path, sync::Arc};
    let with = |value: u64| SystemMetrics {
        cpu_percent: Some(10.0),
        memory: Some(crate::linux::ByteUsage { used: 1, total: 4 }),
        swap: Some(crate::linux::ByteUsage {
            used: value,
            total: 100,
        }),
        mounts: vec![crate::linux::MountUsage {
            mount_point: "/".into(),
            usage: crate::linux::ByteUsage {
                used: value,
                total: 100,
            },
        }],
        gpus: vec![crate::linux::GpuTelemetry {
            device_path: Arc::from(Path::new("/sys/devices/gpu")),
            utilization: Some(value as f64),
            vram: None,
            power_watts: None,
            fan_percent: None,
        }],
        ..SystemMetrics::default()
    };
    let mut app = App::default();
    app.update(Action::SystemMetricsUpdated(with(1)));
    for (offset, tab) in [Tab::Processes, Tab::Services, Tab::Logs, Tab::Network]
        .into_iter()
        .enumerate()
    {
        app.update(Action::SelectTab(tab));
        assert!(
            !app.update(Action::SystemMetricsUpdated(with(2 + offset as u64))),
            "{tab:?}"
        );
    }
    app.update(Action::SelectTab(Tab::Overview));
    assert!(app.update(Action::SystemMetricsUpdated(with(50))));
}

#[test]
fn temperature_changes_redraw_only_the_overview() {
    use crate::linux::{Temperature, TemperatureKey};
    let with_temperature = |celsius| SystemMetrics {
        cpu_percent: Some(10.0),
        memory: Some(crate::linux::ByteUsage { used: 1, total: 4 }),
        temperatures: vec![Temperature {
            key: TemperatureKey::CpuPackage(0),
            celsius: Some(celsius),
            max: None,
            crit: None,
        }],
        ..SystemMetrics::default()
    };
    let mut app = App::default();
    app.update(Action::SystemMetricsUpdated(with_temperature(50)));

    // Hidden Overview: the new value is cached without a redraw.
    for tab in [Tab::Processes, Tab::Services, Tab::Logs, Tab::Network] {
        app.update(Action::SelectTab(tab));
        let celsius = 51 + tab as i16;
        assert!(
            !app.update(Action::SystemMetricsUpdated(with_temperature(celsius))),
            "{tab:?}"
        );
        assert_eq!(app.system_metrics().temperatures[0].celsius, Some(celsius));
    }
    app.update(Action::SelectTab(Tab::Overview));
    assert!(app.update(Action::SystemMetricsUpdated(with_temperature(60))));
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
fn network_history_follows_the_primary_interface_once_per_snapshot() {
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
    // enp6s0 is the Network card's interface: 1000 + 24; wlan0 and the
    // virtual interfaces are not counted.
    assert_eq!(
        app.network_history().iter().collect::<Vec<_>>(),
        [1024.0, 1024.0]
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

    // A new primary interface starts a new history.
    app.update(Action::NetworkUpdated(NetworkSnapshot {
        interfaces: vec![interface("wlan0", Some(7.0), Some(3.0))],
        error: None,
    }));
    assert_eq!(app.network_history().iter().collect::<Vec<_>>(), [10.0]);
}

#[test]
fn sensors_are_visible_only_on_the_overview_and_the_gpu_graph_restarts_there() {
    use std::{path::Path, sync::Arc};
    let path: Arc<Path> = Arc::from(Path::new("/sys/devices/gpu"));
    let mut app = App::default();
    assert!(app.sensors_visible());
    app.update(Action::HardwareDiscovered(
        crate::linux::HardwareInventory {
            gpus: vec![crate::linux::GpuDevice {
                model: "GPU".into(),
                kind: Some(crate::linux::GpuKind::Discrete),
                vram_bytes: None,
                device_path: Some(Arc::clone(&path)),
            }],
            ..crate::linux::HardwareInventory::default()
        },
    ));
    app.update(Action::SystemMetricsUpdated(SystemMetrics {
        gpus: vec![crate::linux::GpuTelemetry {
            device_path: Arc::clone(&path),
            utilization: Some(40.0),
            vram: None,
            power_watts: None,
            fan_percent: None,
        }],
        ..SystemMetrics::default()
    }));
    assert_eq!(app.gpu_history().iter().len(), 1);

    for tab in [Tab::Processes, Tab::Services, Tab::Logs, Tab::Network] {
        app.update(Action::SelectTab(tab));
        assert!(!app.sensors_visible(), "{tab:?}");
    }
    assert_eq!(app.gpu_history().iter().len(), 1, "kept while away");
    app.update(Action::SelectTab(Tab::Overview));
    assert!(app.sensors_visible());
    assert_eq!(app.gpu_history().iter().len(), 0, "restarted on return");
    // Selecting the Overview again while on it keeps the history.
    app.update(Action::SystemMetricsUpdated(SystemMetrics {
        gpus: vec![crate::linux::GpuTelemetry {
            device_path: Arc::clone(&path),
            utilization: Some(50.0),
            vram: None,
            power_watts: None,
            fan_percent: None,
        }],
        ..SystemMetrics::default()
    }));
    app.update(Action::SelectTab(Tab::Overview));
    assert_eq!(app.gpu_history().iter().len(), 1);
}

#[test]
fn gpu_history_records_the_primary_gpu_utilization() {
    use std::{path::Path, sync::Arc};
    let integrated: Arc<Path> = Arc::from(Path::new("/sys/devices/igpu"));
    let discrete: Arc<Path> = Arc::from(Path::new("/sys/devices/dgpu"));
    let gpu = |path: &Arc<Path>, kind| crate::linux::GpuDevice {
        model: "GPU".into(),
        kind: Some(kind),
        vram_bytes: None,
        device_path: Some(Arc::clone(path)),
    };
    let telemetry = |path: &Arc<Path>, utilization| crate::linux::GpuTelemetry {
        utilization: Some(utilization),
        ..crate::linux::GpuTelemetry {
            device_path: Arc::clone(path),
            utilization: None,
            vram: None,
            power_watts: None,
            fan_percent: None,
        }
    };
    let metrics = SystemMetrics {
        gpus: vec![telemetry(&integrated, 5.0), telemetry(&discrete, 60.0)],
        ..SystemMetrics::default()
    };
    let mut app = App::default();
    // Before discovery there is no primary GPU to record.
    assert!(app.update(Action::SystemMetricsUpdated(metrics.clone())));
    assert_eq!(app.gpu_history().iter().len(), 0);

    app.update(Action::HardwareDiscovered(
        crate::linux::HardwareInventory {
            gpus: vec![
                gpu(&integrated, crate::linux::GpuKind::Integrated),
                gpu(&discrete, crate::linux::GpuKind::Discrete),
            ],
            ..crate::linux::HardwareInventory::default()
        },
    ));
    assert!(app.update(Action::SystemMetricsUpdated(metrics)));
    assert_eq!(app.gpu_history().iter().collect::<Vec<_>>(), [60.0]);
}

#[test]
fn aggregate_cpu_history_is_bounded_and_evicts_oldest_samples() {
    let mut app = App::default();
    let sample_count = METRIC_HISTORY_CAPACITY + 5;

    for sample in 0..sample_count {
        app.update(Action::SystemMetricsUpdated(SystemMetrics {
            // Tenths, so every sample stays a valid percentage.
            cpu_percent: Some(sample as f64 / 10.0),
            ..Default::default()
        }));
    }

    let history = app.aggregate_cpu_history().iter().collect::<Vec<_>>();
    assert_eq!(history.len(), METRIC_HISTORY_CAPACITY);
    assert_eq!(history.first(), Some(&0.5));
    assert_eq!(history.last(), Some(&((sample_count - 1) as f64 / 10.0)));
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
    assert_resize_preserves_overlay(&mut app, |app| app.process_signal_confirmation().is_some());

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
