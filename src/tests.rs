use super::*;
use crate::action::{Action, MouseTarget, Tab};
use std::sync::{mpsc, Arc, Mutex};

struct RecordedDrop {
    name: &'static str,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl Drop for RecordedDrop {
    fn drop(&mut self) {
        self.events.lock().unwrap().push(self.name);
    }
}

struct BlockingWorkerDrop {
    started: mpsc::Sender<&'static str>,
    release: mpsc::Receiver<()>,
}

impl Drop for BlockingWorkerDrop {
    fn drop(&mut self) {
        self.started.send("worker").unwrap();
        self.release.recv().unwrap();
    }
}

struct SignalingDrop {
    event: mpsc::Sender<&'static str>,
}

impl Drop for SignalingDrop {
    fn drop(&mut self) {
        self.event.send("terminal").unwrap();
    }
}

fn no_ready_action() -> Option<Action> {
    None
}

#[test]
fn idle_scheduler_does_not_request_continuous_frames() {
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let now = Instant::now();

    assert_eq!(scheduler.deadline(), None);
    assert!(!scheduler.take_due(now + Duration::from_secs(1)));
    assert_eq!(scheduler.deadline(), None);
}

#[test]
fn hover_changes_coalesce_while_app_keeps_latest_target() {
    let mut app = App::default();
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let now = Instant::now();

    for (offset, tab) in [Tab::Overview, Tab::Processes, Tab::Services, Tab::Logs]
        .into_iter()
        .enumerate()
    {
        assert!(app.update(Action::HoverMouseTarget(Some(MouseTarget::Tab(tab)))));
        scheduler.request_hover(now + Duration::from_millis(offset as u64));
    }

    assert_eq!(app.hovered(), Some(&MouseTarget::Tab(Tab::Logs)));
    assert!(!scheduler.take_due(now + HOVER_FRAME_INTERVAL - Duration::from_millis(1)));
    assert!(scheduler.take_due(now + HOVER_FRAME_INTERVAL));
    assert_eq!(scheduler.deadline(), None);
}

#[test]
fn rapid_hover_transitions_are_frame_limited() {
    let mut app = App::default();
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let start = Instant::now();
    let mut transitions = 0;
    let mut redraws = 0;

    for millisecond in 0..100 {
        let tab = if millisecond % 2 == 0 {
            Tab::Overview
        } else {
            Tab::Processes
        };
        let now = start + Duration::from_millis(millisecond);
        if app.update(Action::HoverMouseTarget(Some(MouseTarget::Tab(tab)))) {
            transitions += 1;
            scheduler.request_hover(now);
        }
        redraws += usize::from(scheduler.take_due(now));
    }
    redraws += usize::from(scheduler.take_due(start + Duration::from_millis(132)));

    assert_eq!(transitions, 100);
    assert_eq!(redraws, 3);
}

#[test]
fn intentional_input_and_resize_use_immediate_redraws() {
    for action in [
        Action::SelectTab(Tab::Processes),
        Action::ProcessNext,
        Action::SortProcesses(action::ProcessSortField::Cpu),
        Action::StepSamplingInterval(action::IntervalStep::Shorter),
        Action::Resize,
    ] {
        assert_eq!(RedrawPolicy::for_action(&action), RedrawPolicy::Immediate);
    }
}

#[test]
fn immediate_render_satisfies_a_pending_hover_redraw() {
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    scheduler.request_hover(Instant::now());

    scheduler.rendered(Instant::now());

    assert_eq!(scheduler.deadline(), None);
}

fn queued(actions: Vec<Action>) -> impl FnMut(&App) -> io::Result<Option<Action>> {
    let mut actions = std::collections::VecDeque::from(actions);
    move |_| Ok(actions.pop_front())
}

fn metrics_with_cpu(cpu: f64) -> crate::linux::SystemMetrics {
    crate::linux::SystemMetrics {
        cpu_percent: Some(cpu),
        ..Default::default()
    }
}

fn process_snapshot(pids: &[u32]) -> crate::linux::ProcessSnapshot {
    crate::linux::ProcessSnapshot {
        processes: pids
            .iter()
            .map(|&pid| crate::linux::ProcessInfo {
                pid,
                name: format!("proc{pid}"),
                cpu_percent: None,
                memory_bytes: 0,
                command: None,
                state: "S".into(),
                parent_pid: 1,
                state_code: 'S',
                start_time: u64::from(pid),
                kernel_thread: false,
            })
            .collect(),
        error: None,
    }
}

fn journal_batch(id: u64) -> crate::linux::JournalBatch {
    crate::linux::JournalBatch {
        entries: vec![crate::linux::JournalEntry {
            id,
            timestamp_micros: None,
            local_time: None,
            source: "test".into(),
            priority: Some(6),
            message: format!("entry {id}"),
        }],
        dropped: 0,
        error: None,
    }
}

#[test]
fn background_updates_arriving_together_render_once() {
    let mut app = App::default();
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let now = Instant::now();
    scheduler.rendered(now - Duration::from_secs(1));
    let mut remaining = queued(vec![
        Action::ProcessesUpdated(process_snapshot(&[1, 2])),
        Action::NetworkUpdated(crate::linux::NetworkSnapshot::default()),
    ]);
    let mut pulled = 0;

    let render_now = apply_actions(
        &mut app,
        &mut scheduler,
        Some(Action::SystemMetricsUpdated(metrics_with_cpu(12.0))),
        now,
        |app| {
            pulled += 1;
            remaining(app)
        },
    )
    .unwrap();

    assert!(!render_now);
    assert_eq!(pulled, 3, "all ready snapshots are drained in one turn");
    assert_eq!(app.process_summary().total, 2);
    assert!(
        scheduler.take_due(now),
        "idle background data renders promptly"
    );
    assert!(!scheduler.take_due(now));
}

#[test]
fn inactive_tab_snapshots_do_not_schedule_a_redraw() {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Services));
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);

    let render_now = apply_actions(
        &mut app,
        &mut scheduler,
        Some(Action::ProcessesUpdated(process_snapshot(&[1]))),
        Instant::now(),
        queued(vec![Action::SystemMetricsUpdated(metrics_with_cpu(3.0))]),
    )
    .unwrap();

    assert!(!render_now);
    assert_eq!(scheduler.deadline(), None);
}

#[test]
fn continuous_journal_batches_are_frame_limited() {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Logs));
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let start = Instant::now();
    scheduler.rendered(start);
    let mut renders = 0;

    for millisecond in 1..=1000_u64 {
        let now = start + Duration::from_millis(millisecond);
        let render_now = apply_actions(
            &mut app,
            &mut scheduler,
            Some(Action::LogsUpdated(journal_batch(millisecond))),
            now,
            queued(Vec::new()),
        )
        .unwrap();
        assert!(!render_now);
        if scheduler.take_due(now) {
            renders += 1;
            scheduler.rendered(now);
        }
    }

    assert_eq!(app.log_count(), 1000);
    assert_eq!(renders, 20);
}

#[test]
fn a_continuously_ready_source_cannot_monopolize_a_turn() {
    let mut app = App::default();
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let mut next_id = 0;

    apply_actions(
        &mut app,
        &mut scheduler,
        Some(Action::LogsUpdated(journal_batch(0))),
        Instant::now(),
        |_| {
            next_id += 1;
            Ok(Some(Action::LogsUpdated(journal_batch(next_id))))
        },
    )
    .unwrap();

    assert_eq!(next_id as usize, MAX_ACTIONS_PER_TURN - 1);
}

#[test]
fn state_changing_input_ends_the_turn_for_an_immediate_render() {
    let mut app = App::default();
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let mut remaining = queued(vec![
        Action::SelectTab(Tab::Processes),
        Action::SelectTab(Tab::Logs),
    ]);

    let render_now = apply_actions(
        &mut app,
        &mut scheduler,
        Some(Action::SystemMetricsUpdated(metrics_with_cpu(50.0))),
        Instant::now(),
        &mut remaining,
    )
    .unwrap();

    assert!(render_now);
    assert_eq!(app.active_tab(), Tab::Processes);
    assert_eq!(
        remaining(&app).unwrap(),
        Some(Action::SelectTab(Tab::Logs)),
        "later input waits for regions from the immediate render"
    );
}

#[test]
fn quit_stops_draining_ready_actions() {
    let mut app = App::default();
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let mut pulled = 0;

    let render_now = apply_actions(
        &mut app,
        &mut scheduler,
        Some(Action::Quit),
        Instant::now(),
        |_| {
            pulled += 1;
            Ok(Some(Action::LogsUpdated(journal_batch(1))))
        },
    )
    .unwrap();

    assert!(!render_now);
    assert!(app.should_quit());
    assert_eq!(pulled, 0);
}

#[test]
fn background_snapshots_use_frame_limited_redraws() {
    for action in [
        Action::SystemMetricsUpdated(Default::default()),
        Action::ProcessesUpdated(Default::default()),
        Action::ServicesUpdated(Default::default()),
        Action::NetworkUpdated(Default::default()),
        Action::LogsUpdated(Default::default()),
    ] {
        assert_eq!(
            RedrawPolicy::for_action(&action),
            RedrawPolicy::FrameLimited
        );
    }
}

#[test]
fn pending_hover_frame_is_not_delayed_by_background_requests() {
    let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);
    let now = Instant::now();
    scheduler.rendered(now);

    scheduler.request_hover(now);
    scheduler.request_background(now);

    assert_eq!(scheduler.deadline(), Some(now + HOVER_FRAME_INTERVAL));
}

#[test]
fn continuous_journal_readiness_cannot_delay_quit() {
    let mut journal_polls = 0;

    for _ in 0..100 {
        let action = poll_ready_action(
            || Ok(Some(Action::Quit)),
            no_ready_action,
            no_ready_action,
            no_ready_action,
            no_ready_action,
            no_ready_action,
            || {
                journal_polls += 1;
                Some(Action::LogsUpdated(Default::default()))
            },
        )
        .unwrap();

        assert_eq!(action, Some(Action::Quit));
    }
    assert_eq!(journal_polls, 0);
}

#[test]
fn continuous_journal_readiness_cannot_delay_resize() {
    let mut journal_polls = 0;

    for _ in 0..100 {
        let action = poll_ready_action(
            || Ok(Some(Action::Resize)),
            no_ready_action,
            no_ready_action,
            no_ready_action,
            no_ready_action,
            no_ready_action,
            || {
                journal_polls += 1;
                Some(Action::LogsUpdated(Default::default()))
            },
        )
        .unwrap();

        assert_eq!(action, Some(Action::Resize));
    }
    assert_eq!(journal_polls, 0);
}

#[test]
fn continuous_journal_readiness_cannot_delay_network_updates() {
    let mut journal_polls = 0;

    let action = poll_ready_action(
        || Ok(None),
        no_ready_action,
        no_ready_action,
        no_ready_action,
        || {
            Some(Action::NetworkUpdated(
                crate::linux::NetworkSnapshot::default(),
            ))
        },
        no_ready_action,
        || {
            journal_polls += 1;
            Some(Action::LogsUpdated(Default::default()))
        },
    )
    .unwrap();

    assert!(matches!(action, Some(Action::NetworkUpdated(_))));
    assert_eq!(journal_polls, 0);
}

#[test]
fn a_termination_signal_quits_before_pending_input_and_snapshots() {
    let shutdown = crate::shutdown::Shutdown::default();
    let mut terminal_polls = 0;
    assert_eq!(
        input_action(&shutdown, || {
            terminal_polls += 1;
            Ok(Some(Action::NextTab))
        })
        .unwrap(),
        Some(Action::NextTab)
    );

    shutdown.simulate(signal_hook::consts::SIGTERM);
    let action = poll_ready_action(
        || {
            input_action(&shutdown, || {
                terminal_polls += 1;
                Ok(Some(Action::NextTab))
            })
        },
        || Some(Action::SystemMetricsUpdated(metrics_with_cpu(1.0))),
        no_ready_action,
        no_ready_action,
        no_ready_action,
        no_ready_action,
        no_ready_action,
    )
    .unwrap();

    assert_eq!(action, Some(Action::Quit));
    assert_eq!(
        terminal_polls, 1,
        "pending input is not read after a signal"
    );
}

#[test]
fn should_restore_terminal_for_panic_filters_threads() {
    let main_id = std::thread::current().id();
    assert!(should_restore_terminal_for_panic(main_id, main_id));

    let worker = std::thread::spawn(move || {
        let worker_id = std::thread::current().id();
        assert!(!should_restore_terminal_for_panic(main_id, worker_id));
    });
    worker.join().unwrap();
}

#[test]
fn normal_and_error_results_restore_terminal_before_workers() {
    for run_result in [Ok(()), Err(io::Error::other("main loop failed"))] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let terminal = RecordedDrop {
            name: "terminal",
            events: Arc::clone(&events),
        };
        let worker = RecordedDrop {
            name: "worker",
            events: Arc::clone(&events),
        };

        let expected_ok = run_result.is_ok();
        let result = finish_application(terminal, worker, run_result);

        assert_eq!(*events.lock().unwrap(), ["terminal", "worker"]);
        assert_eq!(result.is_ok(), expected_ok);
    }
}

#[test]
fn quit_uses_normal_terminal_first_teardown() {
    let mut app = App::default();
    assert!(!app.update(Action::Quit));
    assert!(app.should_quit());

    let events = Arc::new(Mutex::new(Vec::new()));
    let terminal = RecordedDrop {
        name: "terminal",
        events: Arc::clone(&events),
    };
    let worker = RecordedDrop {
        name: "worker",
        events: Arc::clone(&events),
    };

    finish_application(terminal, worker, Ok(())).unwrap();

    assert_eq!(*events.lock().unwrap(), ["terminal", "worker"]);
}

#[test]
fn terminal_restoration_precedes_blocked_worker_teardown() {
    let (events_tx, events_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let terminal = SignalingDrop {
        event: events_tx.clone(),
    };
    let worker = BlockingWorkerDrop {
        started: events_tx,
        release: release_rx,
    };

    let teardown = std::thread::spawn(move || {
        finish_application(terminal, worker, Ok(())).unwrap();
        done_tx.send(()).unwrap();
    });

    assert_eq!(events_rx.recv().unwrap(), "terminal");
    assert_eq!(events_rx.recv().unwrap(), "worker");
    assert_eq!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    release_tx.send(()).unwrap();
    teardown.join().unwrap();
    assert_eq!(done_rx.recv().unwrap(), ());
}

#[test]
fn a_worker_that_never_stops_does_not_block_exit() {
    let (events_tx, events_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let terminal = SignalingDrop {
        event: events_tx.clone(),
    };
    let worker = BlockingWorkerDrop {
        started: events_tx,
        release: release_rx,
    };

    let started = Instant::now();
    finish_application_within(terminal, worker, Ok(()), Duration::from_millis(100)).unwrap();

    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(events_rx.recv().unwrap(), "terminal");
    assert_eq!(events_rx.recv().unwrap(), "worker");
    release_tx.send(()).unwrap();
}

#[test]
fn terminal_restore_commands_are_idempotent() {
    let mut buffer = Vec::new();
    assert!(restore_terminal_commands(&mut buffer).is_ok());
    let first_len = buffer.len();
    assert!(first_len > 0);

    assert!(restore_terminal_commands(&mut buffer).is_ok());
    assert_eq!(buffer.len(), first_len * 2);
}
