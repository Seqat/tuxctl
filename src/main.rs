mod action;
mod app;
mod event;
mod linux;
mod ui;

use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};

use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::App;
use event::EventHandler;
use ui::UiRegions;

const TICK_RATE: Duration = Duration::from_millis(250);
const METRICS_REFRESH_RATE: Duration = Duration::from_secs(1);
const SERVICES_REFRESH_RATE: Duration = Duration::from_secs(5);
const HOVER_FRAME_INTERVAL: Duration = Duration::from_millis(33);

fn main() -> io::Result<()> {
    let main_thread = std::thread::current().id();
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if should_restore_terminal_for_panic(main_thread, std::thread::current().id()) {
            restore_terminal(&mut io::stdout());
        }
        default_panic(panic_info);
    }));

    let mut terminal = TerminalSession::new()?;
    let mut app = App::default();
    let mut events = EventHandler::new(TICK_RATE);
    let metrics = match linux::SystemMetricsCollector::start(METRICS_REFRESH_RATE) {
        Ok(metrics) => metrics,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let processes = match linux::ProcessCollector::start(METRICS_REFRESH_RATE) {
        Ok(processes) => processes,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let services = match linux::ServiceCollector::start(SERVICES_REFRESH_RATE) {
        Ok(services) => services,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let journal = linux::JournalCollector::start();
    let network = match linux::NetworkCollector::start(METRICS_REFRESH_RATE) {
        Ok(network) => network,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let hardware = match linux::HardwareCollector::start() {
        Ok(hardware) => hardware,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let mut redraws = RedrawScheduler::new(HOVER_FRAME_INTERVAL);

    let run_result = (|| -> io::Result<()> {
        let mut regions = draw_app(&mut terminal, &mut app)?;

        while !app.should_quit() {
            let action = match poll_ready_action(
                || events.poll_action(&regions, app.hovered()),
                || metrics.latest().map(action::Action::SystemMetricsUpdated),
                || processes.latest().map(action::Action::ProcessesUpdated),
                || services.latest().map(action::Action::ServicesUpdated),
                || network.latest().map(action::Action::NetworkUpdated),
                || hardware.latest().map(action::Action::HardwareDiscovered),
                || journal.latest().map(action::Action::LogsUpdated),
            )? {
                Some(action) => Some(action),
                None => events.next_action(&regions, app.hovered(), redraws.deadline())?,
            };

            if let Some(action) = action {
                let redraw_policy = RedrawPolicy::for_action(&action);
                if app.update(action) {
                    match redraw_policy {
                        RedrawPolicy::CoalescedHover => redraws.request_hover(Instant::now()),
                        RedrawPolicy::Immediate if !app.should_quit() => {
                            regions = draw_app(&mut terminal, &mut app)?;
                            redraws.rendered();
                        }
                        RedrawPolicy::Immediate => {}
                    }
                }
            }

            if let Some(generation) = app.take_service_refresh_request() {
                services.request_refresh(generation);
            }

            if redraws.take_due(Instant::now()) && !app.should_quit() {
                regions = draw_app(&mut terminal, &mut app)?;
            }
        }

        Ok(())
    })();

    finish_application(
        terminal,
        (hardware, network, journal, services, processes, metrics),
        run_result,
    )
}

fn finish_application<T, W, R>(terminal: T, workers: W, result: io::Result<R>) -> io::Result<R> {
    drop(terminal);
    drop(workers);
    result
}

fn poll_ready_action(
    terminal: impl FnOnce() -> io::Result<Option<action::Action>>,
    metrics: impl FnOnce() -> Option<action::Action>,
    processes: impl FnOnce() -> Option<action::Action>,
    services: impl FnOnce() -> Option<action::Action>,
    network: impl FnOnce() -> Option<action::Action>,
    hardware: impl FnOnce() -> Option<action::Action>,
    journal: impl FnOnce() -> Option<action::Action>,
) -> io::Result<Option<action::Action>> {
    if let Some(action) = terminal()? {
        return Ok(Some(action));
    }
    if let Some(action) = metrics() {
        return Ok(Some(action));
    }
    if let Some(action) = processes() {
        return Ok(Some(action));
    }
    if let Some(action) = services() {
        return Ok(Some(action));
    }
    if let Some(action) = network() {
        return Ok(Some(action));
    }
    if let Some(action) = hardware() {
        return Ok(Some(action));
    }
    Ok(journal())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedrawPolicy {
    Immediate,
    CoalescedHover,
}

impl RedrawPolicy {
    fn for_action(action: &action::Action) -> Self {
        if matches!(action, action::Action::HoverMouseTarget(_)) {
            Self::CoalescedHover
        } else {
            Self::Immediate
        }
    }
}

#[derive(Debug)]
struct RedrawScheduler {
    hover_frame_interval: Duration,
    hover_deadline: Option<Instant>,
}

impl RedrawScheduler {
    fn new(hover_frame_interval: Duration) -> Self {
        Self {
            hover_frame_interval,
            hover_deadline: None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.hover_deadline
    }

    fn request_hover(&mut self, now: Instant) {
        self.hover_deadline
            .get_or_insert(now + self.hover_frame_interval);
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.hover_deadline.is_some_and(|deadline| now >= deadline) {
            self.hover_deadline = None;
            true
        } else {
            false
        }
    }

    fn rendered(&mut self) {
        self.hover_deadline = None;
    }
}

fn draw_app(terminal: &mut TerminalSession, app: &mut App) -> io::Result<UiRegions> {
    let regions = terminal.draw(app)?;
    if let Some((start, height)) = regions.process_viewport() {
        app.update(action::Action::ProcessViewportChanged { start, height });
    }
    if let Some((start, height)) = regions.service_viewport() {
        app.update(action::Action::ServiceViewportChanged { start, height });
    }
    if let Some((start, height)) = regions.log_viewport() {
        app.update(action::Action::LogViewportChanged { start, height });
    }
    if let Some((start, height)) = regions.network_viewport() {
        app.update(action::Action::NetworkViewportChanged { start, height });
    }
    Ok(regions)
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

struct TerminalSession {
    terminal: Tui,
}

impl TerminalSession {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        if let Err(error) = execute!(stdout, EnableMouseCapture) {
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error);
        }

        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                restore_terminal(&mut io::stdout());
                Err(error)
            }
        }
    }

    fn draw(&mut self, app: &App) -> io::Result<UiRegions> {
        let mut regions = UiRegions::default();
        self.terminal
            .draw(|frame| regions = ui::render(frame, app))?;
        Ok(regions)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore_terminal(self.terminal.backend_mut());
    }
}

fn restore_terminal_commands<W: io::Write>(output: &mut W) -> io::Result<()> {
    execute!(output, DisableMouseCapture, LeaveAlternateScreen, Show)
}

fn restore_terminal<W: io::Write>(output: &mut W) {
    let _ = restore_terminal_commands(output);
    let _ = disable_raw_mode();
}

fn should_restore_terminal_for_panic(
    main_thread: std::thread::ThreadId,
    panicking_thread: std::thread::ThreadId,
) -> bool {
    panicking_thread == main_thread
}

#[cfg(test)]
mod tests {
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
        let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL);
        let now = Instant::now();

        assert_eq!(scheduler.deadline(), None);
        assert!(!scheduler.take_due(now + Duration::from_secs(1)));
        assert_eq!(scheduler.deadline(), None);
    }

    #[test]
    fn hover_changes_coalesce_while_app_keeps_latest_target() {
        let mut app = App::default();
        let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL);
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
        let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL);
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
            Action::Resize,
        ] {
            assert_eq!(RedrawPolicy::for_action(&action), RedrawPolicy::Immediate);
        }
    }

    #[test]
    fn immediate_render_satisfies_a_pending_hover_redraw() {
        let mut scheduler = RedrawScheduler::new(HOVER_FRAME_INTERVAL);
        scheduler.request_hover(Instant::now());

        scheduler.rendered();

        assert_eq!(scheduler.deadline(), None);
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
    fn terminal_restore_commands_are_idempotent() {
        let mut buffer = Vec::new();
        assert!(restore_terminal_commands(&mut buffer).is_ok());
        let first_len = buffer.len();
        assert!(first_len > 0);

        assert!(restore_terminal_commands(&mut buffer).is_ok());
        assert_eq!(buffer.len(), first_len * 2);
    }
}
