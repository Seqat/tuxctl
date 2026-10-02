//! `tuxctl`: a lightweight, keyboard-first Linux TUI for system monitoring and
//! management.
//!
//! This is the entry point and the event loop. It parses the command line, owns
//! the terminal for the whole session (restored on a normal exit, on SIGTERM,
//! SIGHUP and SIGINT, and on a panic of the main thread; a worker's panic never
//! touches it), and starts the background collectors (Services paused until
//! its tab is visible, the journal on the first visit to Logs). The loop
//! turns terminal events and collector snapshots into `Action`s, applies them
//! with `App::update`, and redraws from cached state only when something
//! visible changed.

// Every `unsafe` block states why it is sound (`// SAFETY: …`).
#![deny(clippy::undocumented_unsafe_blocks)]
// Outside tests, errors are handled rather than turned into panics: a panic
// in the UI thread ends the session.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

mod about;
mod action;
mod app;
mod check;
mod cli;
mod event;
mod keymap;
mod linux;
mod shutdown;
mod text;
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

use app::{App, CollectorPeriods};
use event::EventHandler;
use ui::UiRegions;

const TICK_RATE: Duration = Duration::from_millis(250);
const HOVER_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const BACKGROUND_FRAME_INTERVAL: Duration = Duration::from_millis(50);
/// Upper bound on actions applied before the loop renders or blocks again.
const MAX_ACTIONS_PER_TURN: usize = 16;
/// How long quitting waits for the background workers to stop.
const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> io::Result<()> {
    // Arguments are handled before the terminal is touched.
    let (interval, nvidia_temperature) = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Command::Run {
            interval,
            nvidia_temperature,
        }) => (interval, nvidia_temperature),
        Ok(cli::Command::Check { nvidia_temperature }) => {
            print!(
                "{}",
                check::report_text(&linux::sensor_report(nvidia_temperature))
            );
            return Ok(());
        }
        Ok(cli::Command::Help) => {
            print!("{}", cli::help_text());
            return Ok(());
        }
        Ok(cli::Command::Version) => {
            println!("{}", cli::version_text());
            return Ok(());
        }
        Err(message) => {
            eprintln!("tuxctl: {message}\n{}", cli::USAGE);
            std::process::exit(2);
        }
    };
    let periods = CollectorPeriods::for_sampling_interval(interval);
    ui::init_color_depth(ui::ColorDepth::detect(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    ));
    let shutdown = shutdown::Shutdown::install()?;

    let main_thread = std::thread::current().id();
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if should_restore_terminal_for_panic(main_thread, std::thread::current().id()) {
            restore_terminal(&mut io::stdout());
        }
        default_panic(panic_info);
    }));

    let mut terminal = TerminalSession::new()?;
    let mut app = App::default()
        .with_collector_periods(periods)
        .with_nvidia_temperature(nvidia_temperature);
    let mut events = EventHandler::new(TICK_RATE);
    let metrics = match linux::SystemMetricsCollector::start(periods.metrics, nvidia_temperature) {
        Ok(metrics) => metrics,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let processes = match linux::ProcessCollector::start(periods.processes) {
        Ok(processes) => processes,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    // Services only collect while their tab is visible; journalctl starts on
    // the first visit to Logs.
    let mut sensors_active = app.sensors_visible();
    let mut services_paused = !app.services_visible();
    let services = match linux::ServiceCollector::start(periods.services, services_paused) {
        Ok(services) => services,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let mut journal: Option<linux::JournalCollector> = None;
    let network = match linux::NetworkCollector::start(periods.network) {
        Ok(network) => network,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let hardware = match linux::HardwareCollector::start() {
        Ok(hardware) => hardware,
        Err(error) => return finish_application(terminal, (), Err(error)),
    };
    let mut redraws = RedrawScheduler::new(HOVER_FRAME_INTERVAL, BACKGROUND_FRAME_INTERVAL);

    let run_result = (|| -> io::Result<()> {
        let mut regions = draw_app(&mut terminal, &mut app)?;
        redraws.rendered(Instant::now());

        while !app.should_quit() {
            let ready = |events: &mut EventHandler, hovered: Option<&action::MouseTarget>| {
                poll_ready_action(
                    || input_action(&shutdown, || events.poll_action(&regions, hovered)),
                    || metrics.latest().map(action::Action::SystemMetricsUpdated),
                    || processes.latest().map(action::Action::ProcessesUpdated),
                    || services.latest().map(action::Action::ServicesUpdated),
                    || network.latest().map(action::Action::NetworkUpdated),
                    || hardware.latest().map(action::Action::HardwareDiscovered),
                    || {
                        journal
                            .as_ref()
                            .and_then(linux::JournalCollector::latest)
                            .map(action::Action::LogsUpdated)
                    },
                )
            };
            let first = match ready(&mut events, app.hovered())? {
                Some(action) => Some(action),
                None => events.next_action(&regions, app.hovered(), redraws.deadline())?,
            };
            let render_now = apply_actions(&mut app, &mut redraws, first, Instant::now(), |app| {
                ready(&mut events, app.hovered())
            })?;

            if let Some(periods) = app.take_sampling_interval_change() {
                metrics.set_period(periods.metrics);
                processes.set_period(periods.processes);
                network.set_period(periods.network);
                services.set_period(periods.services);
            }
            // Queue the tab-entry refresh before resuming so the worker wakes
            // to exactly one collection.
            if let Some(generation) = app.take_service_refresh_request() {
                services.request_refresh(generation);
            }
            if sensors_active != app.sensors_visible() {
                sensors_active = !sensors_active;
                metrics.set_sensors_active(sensors_active);
            }
            if services_paused == app.services_visible() {
                services_paused = !services_paused;
                services.set_paused(services_paused);
            }
            if journal.is_none() && app.logs_visited() {
                journal = Some(linux::JournalCollector::start());
            }

            if app.should_quit() {
                break;
            }
            let now = Instant::now();
            if render_now || redraws.take_due(now) {
                regions = draw_app(&mut terminal, &mut app)?;
                redraws.rendered(now);
            }
        }

        Ok(())
    })();

    let result = finish_application(
        terminal,
        (hardware, network, journal, services, processes, metrics),
        run_result,
    );
    // After a signal the terminal is restored and the workers have stopped;
    // end the way the signal would have, even if the loop ended in an error
    // (after SIGHUP the terminal is usually gone).
    if let Some(signal) = shutdown.requested() {
        shutdown::terminate(signal)?;
    }
    result
}

/// Restores the terminal, then stops the workers. A worker can be stuck in a
/// read that another local user controls (a `/proc/<pid>/cmdline` read waits
/// on that process's memory lock), so the stop is awaited for at most
/// `WORKER_STOP_TIMEOUT`; the process then exits and takes the thread with it.
fn finish_application<T, W, R>(terminal: T, workers: W, result: io::Result<R>) -> io::Result<R>
where
    W: Send + 'static,
{
    finish_application_within(terminal, workers, result, WORKER_STOP_TIMEOUT)
}

fn finish_application_within<T, W, R>(
    terminal: T,
    workers: W,
    result: io::Result<R>,
    timeout: Duration,
) -> io::Result<R>
where
    W: Send + 'static,
{
    drop(terminal);
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
    let stopper = std::thread::Builder::new()
        .name("stop-workers".into())
        .spawn(move || {
            drop(workers);
            let _ = stopped_tx.send(());
        });
    if stopper.is_ok() {
        let _ = stopped_rx.recv_timeout(timeout);
    }
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

/// Input source for the main loop: a termination signal becomes `Quit` and
/// outranks pending terminal input.
fn input_action(
    shutdown: &shutdown::Shutdown,
    terminal: impl FnOnce() -> io::Result<Option<action::Action>>,
) -> io::Result<Option<action::Action>> {
    match shutdown.requested() {
        Some(_) => Ok(Some(action::Action::Quit)),
        None => terminal(),
    }
}

/// Applies `first` and then further ready actions, up to [`MAX_ACTIONS_PER_TURN`].
///
/// Background updates only schedule a frame-limited redraw, so several snapshots
/// arriving together cost one render. The first state-changing input action ends
/// the turn and returns `true` so it is rendered immediately and later input is
/// translated against regions that match the screen.
fn apply_actions(
    app: &mut App,
    redraws: &mut RedrawScheduler,
    first: Option<action::Action>,
    now: Instant,
    mut next_ready: impl FnMut(&App) -> io::Result<Option<action::Action>>,
) -> io::Result<bool> {
    let mut next = first;
    let mut applied = 0;
    while let Some(action) = next {
        let redraw_policy = RedrawPolicy::for_action(&action);
        applied += 1;
        if app.update(action) {
            match redraw_policy {
                RedrawPolicy::Immediate => return Ok(true),
                RedrawPolicy::CoalescedHover => redraws.request_hover(now),
                RedrawPolicy::FrameLimited => redraws.request_background(now),
            }
        }
        if app.should_quit() || applied >= MAX_ACTIONS_PER_TURN {
            break;
        }
        next = next_ready(app)?;
    }
    Ok(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedrawPolicy {
    Immediate,
    CoalescedHover,
    FrameLimited,
}

impl RedrawPolicy {
    fn for_action(action: &action::Action) -> Self {
        use action::Action;
        match action {
            Action::HoverMouseTarget(_) => Self::CoalescedHover,
            Action::SystemMetricsUpdated(_)
            | Action::ProcessesUpdated(_)
            | Action::ServicesUpdated(_)
            | Action::NetworkUpdated(_)
            | Action::HardwareDiscovered(_)
            | Action::LogsUpdated(_) => Self::FrameLimited,
            _ => Self::Immediate,
        }
    }
}

#[derive(Debug)]
struct RedrawScheduler {
    hover_frame_interval: Duration,
    background_frame_interval: Duration,
    deadline: Option<Instant>,
    last_render: Option<Instant>,
}

impl RedrawScheduler {
    fn new(hover_frame_interval: Duration, background_frame_interval: Duration) -> Self {
        Self {
            hover_frame_interval,
            background_frame_interval,
            deadline: None,
            last_render: None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn request_hover(&mut self, now: Instant) {
        self.schedule(now + self.hover_frame_interval);
    }

    /// Renders background data promptly when idle, but at most once per
    /// `background_frame_interval` while updates keep arriving.
    fn request_background(&mut self, now: Instant) {
        let earliest = self
            .last_render
            .map_or(now, |rendered| rendered + self.background_frame_interval);
        self.schedule(earliest.max(now));
    }

    fn schedule(&mut self, at: Instant) {
        self.deadline = Some(self.deadline.map_or(at, |deadline| deadline.min(at)));
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            true
        } else {
            false
        }
    }

    fn rendered(&mut self, now: Instant) {
        self.deadline = None;
        self.last_render = Some(now);
    }
}

fn draw_app(terminal: &mut TerminalSession, app: &mut App) -> io::Result<UiRegions> {
    let regions = terminal.draw(app)?;
    #[cfg(feature = "redraw-counter")]
    redraw_counter::record();
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

/// Development-only render counter for `scripts/measure.py`.
#[cfg(feature = "redraw-counter")]
mod redraw_counter {
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicU64, Ordering},
            OnceLock,
        },
    };

    static RENDERS: AtomicU64 = AtomicU64::new(0);
    static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

    /// Counts one render and rewrites the running total to `$TUXCTL_REDRAW_LOG`.
    pub(super) fn record() {
        let renders = RENDERS.fetch_add(1, Ordering::Relaxed) + 1;
        let path =
            LOG_PATH.get_or_init(|| std::env::var_os("TUXCTL_REDRAW_LOG").map(PathBuf::from));
        if let Some(path) = path {
            let _ = std::fs::write(path, renders.to_string());
        }
    }
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
mod tests;
