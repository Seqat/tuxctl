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
    let metrics = linux::SystemMetricsCollector::start(METRICS_REFRESH_RATE)?;
    let processes = linux::ProcessCollector::start(METRICS_REFRESH_RATE)?;
    let services = linux::ServiceCollector::start(SERVICES_REFRESH_RATE)?;
    let journal = linux::JournalCollector::start();
    let network = linux::NetworkCollector::start(METRICS_REFRESH_RATE)?;
    let hardware = linux::HardwareCollector::start()?;
    let mut redraws = RedrawScheduler::new(HOVER_FRAME_INTERVAL);

    let mut regions = draw_app(&mut terminal, &mut app)?;

    while !app.should_quit() {
        let action = if let Some(user_action) = events.poll_action(&regions, app.hovered())? {
            Some(user_action)
        } else if let Some(metrics) = metrics.latest() {
            Some(action::Action::SystemMetricsUpdated(metrics))
        } else if let Some(snapshot) = processes.latest() {
            Some(action::Action::ProcessesUpdated(snapshot))
        } else if let Some(snapshot) = services.latest() {
            Some(action::Action::ServicesUpdated(snapshot))
        } else if let Some(batch) = journal.latest() {
            Some(action::Action::LogsUpdated(batch))
        } else if let Some(snapshot) = network.latest() {
            Some(action::Action::NetworkUpdated(snapshot))
        } else if let Some(inventory) = hardware.latest() {
            Some(action::Action::HardwareDiscovered(inventory))
        } else {
            events.next_action(&regions, app.hovered(), redraws.deadline())?
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

        if app.take_service_refresh_request() {
            services.request_refresh();
        }

        if redraws.take_due(Instant::now()) && !app.should_quit() {
            regions = draw_app(&mut terminal, &mut app)?;
        }
    }

    drop(terminal);
    Ok(())
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
    fn terminal_restore_commands_are_idempotent() {
        let mut buffer = Vec::new();
        assert!(restore_terminal_commands(&mut buffer).is_ok());
        let first_len = buffer.len();
        assert!(first_len > 0);

        assert!(restore_terminal_commands(&mut buffer).is_ok());
        assert_eq!(buffer.len(), first_len * 2);
    }
}
