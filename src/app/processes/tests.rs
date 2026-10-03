use super::super::test_support::*;
use super::*;

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

fn identity(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_time: u64::from(pid),
    }
}

fn pin(app: &mut App, pid: u32) {
    app.update(Action::SelectProcess(identity(pid)));
    assert_eq!(app.selected_process_identity(), Some(identity(pid)));
    assert!(app.update(Action::TogglePin), "pin {pid}");
}

/// Processes 1..=count sorted by CPU descending: pid 1 lowest CPU.
fn cpu_ranked(count: u32) -> Vec<ProcessInfo> {
    (1..=count)
        .map(|pid| process_with(pid, &format!("p{pid}"), Some(f64::from(pid)), 100))
        .collect()
}

fn processes_app(count: u32) -> App {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    app.update(Action::ProcessesUpdated(processes(cpu_ranked(count))));
    app
}

#[test]
fn pinned_processes_lead_in_pin_order_without_duplicates() {
    let mut app = processes_app(5);
    assert_eq!(visible_pids(&app), vec![5, 4, 3, 2, 1]);

    pin(&mut app, 2);
    pin(&mut app, 4);

    assert_eq!(visible_pids(&app), vec![2, 4, 5, 3, 1]);
    assert_eq!(app.pinned_row_count(), 2);
    assert!(app.process_row_at(0).unwrap().pinned);
    assert!(!app.process_row_at(2).unwrap().pinned);
    assert_eq!(app.listed_process_count(), 5);
}

#[test]
fn sorting_and_refresh_reorder_only_the_unpinned_section() {
    let mut app = processes_app(5);
    pin(&mut app, 1);
    pin(&mut app, 5);

    app.update(Action::SortProcesses(ProcessSortField::Pid));
    assert_eq!(visible_pids(&app), vec![1, 5, 2, 3, 4]);
    app.update(Action::SortProcesses(ProcessSortField::Pid));
    assert_eq!(visible_pids(&app), vec![1, 5, 4, 3, 2]);

    let mut refreshed = cpu_ranked(5);
    refreshed.reverse();
    app.update(Action::ProcessesUpdated(processes(refreshed)));
    assert_eq!(visible_pids(&app), vec![1, 5, 4, 3, 2]);

    // Hidden snapshots defer the rebuild; the pins survive it.
    app.update(Action::SelectTab(Tab::Logs));
    app.update(Action::ProcessesUpdated(processes(cpu_ranked(6))));
    app.update(Action::SelectTab(Tab::Processes));
    assert_eq!(visible_pids(&app), vec![1, 5, 6, 4, 3, 2]);
}

#[test]
fn unpinning_returns_a_process_to_its_sorted_place() {
    let mut app = processes_app(4);
    pin(&mut app, 1);
    assert_eq!(visible_pids(&app), vec![1, 4, 3, 2]);

    assert!(app.update(Action::TogglePin));

    assert_eq!(visible_pids(&app), vec![4, 3, 2, 1]);
    assert_eq!(app.pinned_row_count(), 0);
    assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
}

#[test]
fn the_pin_limit_is_enforced_with_a_message() {
    let mut app = processes_app(MAX_PINNED_PROCESSES as u32 + 2);
    for pid in 1..=MAX_PINNED_PROCESSES as u32 {
        pin(&mut app, pid);
    }
    app.update(Action::SelectProcess(identity(
        MAX_PINNED_PROCESSES as u32 + 1,
    )));

    assert!(app.update(Action::TogglePin), "the message is shown");
    assert_eq!(app.pinned_row_count(), MAX_PINNED_PROCESSES);
    assert_eq!(app.process_action_message(), Some("Pin limit (8) reached"));
}

#[test]
fn a_reused_pid_never_inherits_a_pin() {
    let mut app = processes_app(3);
    pin(&mut app, 2);

    let mut reused = process(2, "impostor");
    reused.start_time = 999;
    app.update(Action::ProcessesUpdated(processes(vec![
        process(1, "p1"),
        reused,
        process(3, "p3"),
    ])));

    let pinned = app.process_row_at(0).unwrap();
    assert!(pinned.exited, "the pinned identity is gone");
    assert_eq!(pinned.process.name, "p2");
    let impostor = (1..app.process_count())
        .filter_map(|index| app.process_row_at(index))
        .find(|row| row.process.name == "impostor")
        .expect("the new process is listed");
    assert!(!impostor.pinned);
}

#[test]
fn exited_pins_linger_then_drop_with_one_redraw_when_visible() {
    let mut app = processes_app(3);
    pin(&mut app, 2);
    app.update(Action::ProcessesUpdated(processes(vec![
        process(1, "p1"),
        process(3, "p3"),
    ])));
    assert!(app.process_row_at(0).unwrap().exited);

    let start = Instant::now();
    assert!(!app.update(Action::Tick(start)), "linger starts");
    assert!(!app.update(Action::Tick(start + Duration::from_secs(4))));
    assert!(app.process_row_at(0).unwrap().exited);

    assert!(app.update(Action::Tick(start + EXITED_PIN_LINGER)));
    assert_eq!(app.pinned_row_count(), 0);
    assert_eq!(visible_pids(&app), vec![1, 3]);
    assert!(!app.update(Action::Tick(start + EXITED_PIN_LINGER * 2)));
}

#[test]
fn exited_pins_dropped_on_another_tab_do_not_redraw_it() {
    // Logs shows no pins; Overview lists them, so it does redraw.
    for (tab, redraws) in [(Tab::Logs, false), (Tab::Overview, true)] {
        let mut app = processes_app(3);
        pin(&mut app, 2);
        app.update(Action::SelectTab(tab));
        app.update(Action::ProcessesUpdated(processes(vec![process(1, "p1")])));

        let start = Instant::now();
        app.update(Action::Tick(start));
        assert_eq!(
            app.update(Action::Tick(start + EXITED_PIN_LINGER)),
            redraws,
            "{tab:?}"
        );
        app.update(Action::SelectTab(Tab::Processes));
        assert_eq!(visible_pids(&app), vec![1]);
    }
}

#[test]
fn a_failed_snapshot_does_not_mark_pins_exited() {
    let mut app = processes_app(3);
    pin(&mut app, 2);

    app.update(Action::ProcessesUpdated(ProcessSnapshot {
        processes: Vec::new(),
        error: Some("proc unavailable".into()),
    }));

    let row = app.process_row_at(0).unwrap();
    assert!(row.pinned && !row.exited);
}

#[test]
fn exited_pinned_rows_refuse_details_and_signals_but_can_be_unpinned() {
    let mut app = processes_app(3);
    pin(&mut app, 2);
    app.update(Action::ProcessesUpdated(processes(vec![
        process(1, "p1"),
        process(3, "p3"),
    ])));
    assert_eq!(app.selected_process_identity(), Some(identity(2)));

    assert!(!app.update(Action::OpenProcessDetails));
    assert!(!app.update(Action::RequestProcessSignal(ProcessSignal::Term)));
    assert!(!app.update(Action::RequestProcessSignal(ProcessSignal::Kill)));
    assert!(app.process_signal_confirmation().is_none());

    assert!(app.update(Action::TogglePin));
    assert_eq!(visible_pids(&app), vec![1, 3]);
}

#[test]
fn an_open_detail_closes_when_its_pinned_process_exits() {
    let mut app = processes_app(2);
    pin(&mut app, 1);
    app.update(Action::OpenProcessDetails);
    assert!(app.process_detail_visible());

    app.update(Action::ProcessesUpdated(processes(vec![process(2, "p2")])));

    assert!(!app.process_detail_visible());
}

#[test]
fn search_dims_non_matching_pins_and_never_falls_back_to_them() {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    app.update(Action::ProcessesUpdated(processes(vec![
        process(1, "bash"),
        process(2, "sshd"),
        process(3, "ssh-agent"),
    ])));
    pin(&mut app, 1);

    app.update(Action::BeginProcessSearch);
    app.update(Action::AppendProcessSearch('s'));
    app.update(Action::AppendProcessSearch('s'));

    assert_eq!(visible_pids(&app), vec![1, 2, 3], "the pin stays visible");
    assert!(app.process_row_at(0).unwrap().dimmed);
    assert_eq!(app.listed_process_count(), 2);
    assert_eq!(
        app.selected_process().map(|p| p.pid),
        Some(2),
        "the selection left the dimmed pin for the first match"
    );

    // Arrows cross into the pinned section; Enter opens the chosen row.
    assert!(app.update(Action::ProcessPrevious));
    assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
    assert!(app.update(Action::OpenProcessDetails));
    assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
}

#[test]
fn a_dimmed_pin_selected_on_purpose_stays_selected_across_refreshes() {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    let snapshot = vec![process(1, "bash"), process(2, "sshd")];
    app.update(Action::ProcessesUpdated(processes(snapshot.clone())));
    pin(&mut app, 1);
    app.update(Action::BeginProcessSearch);
    app.update(Action::AppendProcessSearch('s'));
    app.update(Action::ProcessPrevious);
    assert_eq!(app.selected_process().map(|p| p.pid), Some(1));

    let mut refreshed = snapshot;
    refreshed[1].cpu_percent = Some(50.0);
    app.update(Action::ProcessesUpdated(processes(refreshed)));

    assert_eq!(app.selected_process().map(|p| p.pid), Some(1));
}

#[test]
fn a_search_without_matches_selects_nothing_rather_than_a_dimmed_pin() {
    let mut app = processes_app(3);
    pin(&mut app, 2);
    app.update(Action::SelectProcess(identity(3)));

    app.update(Action::BeginProcessSearch);
    app.update(Action::AppendProcessSearch('z'));

    assert_eq!(visible_pids(&app), vec![2]);
    assert_eq!(app.selected_process_identity(), None);
    assert!(!app.update(Action::OpenProcessDetails));
}

#[test]
fn moving_pins_reorders_them_and_the_selection_follows() {
    let mut app = processes_app(4);
    pin(&mut app, 1);
    pin(&mut app, 2);
    pin(&mut app, 3);
    assert_eq!(visible_pids(&app), vec![1, 2, 3, 4]);

    assert!(app.update(Action::MoveSelectedPin(PinMove::Up)));
    assert_eq!(visible_pids(&app), vec![1, 3, 2, 4]);
    assert!(app.update(Action::MoveSelectedPin(PinMove::Up)));
    assert_eq!(visible_pids(&app), vec![3, 1, 2, 4]);
    assert_eq!(app.selected_process_index(), Some(0));
    assert!(!app.update(Action::MoveSelectedPin(PinMove::Up)), "top");

    app.update(Action::SelectProcess(identity(2)));
    assert!(
        !app.update(Action::MoveSelectedPin(PinMove::Down)),
        "bottom"
    );
    app.update(Action::SelectProcess(identity(4)));
    assert!(
        !app.update(Action::MoveSelectedPin(PinMove::Up)),
        "unpinned rows do not move"
    );
}

#[test]
fn pin_actions_need_the_processes_tab_and_no_overlay() {
    let mut app = processes_app(3);
    pin(&mut app, 1);
    pin(&mut app, 2);

    app.update(Action::SelectTab(Tab::Overview));
    assert!(!app.update(Action::TogglePin));
    assert!(!app.update(Action::MoveSelectedPin(PinMove::Up)));

    app.update(Action::SelectTab(Tab::Processes));
    for open in [
        Action::ShowHelp,
        Action::OpenProcessDetails,
        Action::RequestProcessSignal(ProcessSignal::Term),
    ] {
        assert!(app.update(open.clone()), "{open:?}");
        assert!(!app.update(Action::TogglePin), "{open:?}");
        assert!(
            !app.update(Action::MoveSelectedPin(PinMove::Up)),
            "{open:?}"
        );
        app.update(Action::Escape);
    }
    assert_eq!(visible_pids(&app), vec![1, 2, 3]);
}

#[test]
fn a_pending_signal_keeps_its_target_while_rows_change() {
    let temp_dir = std::env::temp_dir().join(format!("tuxctl_pin_signal_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(temp_dir.join("2")).unwrap();
    std::fs::write(
        temp_dir.join("2/stat"),
        "2 (p2) S 1 2 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 2 0 0 0 0 0 0 0 0 0 0 0 0",
    )
    .unwrap();

    let mut app = processes_app(3);
    pin(&mut app, 3);
    pin(&mut app, 2);
    assert!(app.update(Action::RequestProcessSignal(ProcessSignal::Term)));

    // A refresh reorders everything and a pin move is attempted.
    let mut refreshed = cpu_ranked(4);
    refreshed.reverse();
    app.update(Action::ProcessesUpdated(processes(refreshed)));
    assert!(!app.update(Action::MoveSelectedPin(PinMove::Up)));
    assert!(!app.update(Action::SelectProcess(identity(1))));

    let mut sent_to = None;
    app.confirm_process_signal_at(&temp_dir, Ok, |pid, _| {
        sent_to = Some(*pid);
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&temp_dir);
    assert_eq!(sent_to, Some(2));
    assert_eq!(app.process_action_message(), Some("Sent SIGTERM to p2 (2)"));
}

#[test]
fn the_view_filter_hides_kernel_threads_but_only_dims_a_pinned_one() {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    let kthread = |pid, name| ProcessInfo {
        kernel_thread: true,
        ..process(pid, name)
    };
    app.update(Action::ProcessesUpdated(processes(vec![
        kthread(1, "kworker/0:1"),
        process(2, "bash"),
        kthread(3, "ksoftirqd/0"),
    ])));
    pin(&mut app, 3);

    assert!(app.update(Action::CycleViewFilter));
    assert_eq!(app.process_view_label(), Some("no kernel threads"));
    assert_eq!(visible_pids(&app), vec![3, 2]);
    assert!(app.process_row_at(0).unwrap().dimmed);
    assert_eq!(
        app.selected_process().map(|p| p.pid),
        Some(2),
        "the selection leaves the dimmed pin"
    );

    assert!(app.update(Action::CycleViewFilter));
    assert_eq!(app.process_view_label(), None);
    assert_eq!(visible_pids(&app), vec![3, 1, 2]);
}

#[test]
fn pinned_processes_are_available_while_the_processes_tab_is_hidden() {
    let mut app = processes_app(4);
    pin(&mut app, 3);
    pin(&mut app, 1);
    app.update(Action::SelectTab(Tab::Overview));
    app.update(Action::ProcessesUpdated(processes(vec![
        process(3, "p3"),
        process(4, "p4"),
    ])));

    let pinned: Vec<_> = app
        .pinned_processes()
        .map(|row| (row.process.pid, row.exited))
        .collect();
    assert_eq!(pinned, [(3, false), (1, true)]);
    assert_eq!(app.process_count(), 0, "the table itself stays deferred");
}

#[test]
fn pinned_value_changes_redraw_the_overview_only() {
    let mut app = processes_app(3);
    pin(&mut app, 2);
    app.update(Action::SelectTab(Tab::Overview));
    let with_cpu = |cpu| {
        processes(
            cpu_ranked(3)
                .into_iter()
                .map(|mut p| {
                    if p.pid == 2 {
                        p.cpu_percent = Some(cpu);
                    }
                    p
                })
                .collect(),
        )
    };
    app.update(Action::ProcessesUpdated(with_cpu(2.0)));

    assert!(
        app.update(Action::ProcessesUpdated(with_cpu(40.0))),
        "pinned value changed"
    );
    let mut unpinned_only = cpu_ranked(3);
    unpinned_only[0].cpu_percent = Some(77.0);
    unpinned_only[1].cpu_percent = Some(40.0);
    assert!(
        !app.update(Action::ProcessesUpdated(processes(unpinned_only))),
        "an unpinned change with the same summary is not shown on Overview"
    );

    app.update(Action::SelectTab(Tab::Logs));
    assert!(!app.update(Action::ProcessesUpdated(with_cpu(90.0))));
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
