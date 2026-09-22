use std::{
    io::{self, Read},
    process::{Child, Command, Output, Stdio},
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    control::{CollectorControl, CollectorWake},
    latest_snapshot::{self, LatestReceiver},
};

pub type ServiceRefreshGeneration = u64;

/// Upper bound for one `systemctl` listing; a hung D-Bus call must not wedge the collector.
pub const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceInfo {
    pub unit: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceSnapshot {
    pub services: Vec<ServiceInfo>,
    pub error: Option<String>,
    pub completed_refresh_generation: ServiceRefreshGeneration,
}

#[derive(Default)]
struct ServiceRefreshProgress {
    completed_generation: ServiceRefreshGeneration,
}

impl ServiceRefreshProgress {
    fn finish_collection(
        &mut self,
        mut snapshot: ServiceSnapshot,
        refresh_generation: Option<ServiceRefreshGeneration>,
    ) -> ServiceSnapshot {
        if let Some(generation) = refresh_generation {
            self.completed_generation = self.completed_generation.max(generation);
        }
        snapshot.completed_refresh_generation = self.completed_generation;
        snapshot
    }
}

pub struct ServiceCollector {
    receiver: LatestReceiver<ServiceSnapshot>,
    control: Arc<CollectorControl>,
    worker: Option<JoinHandle<()>>,
}

impl ServiceCollector {
    /// Starts the collector, optionally paused so no `systemctl` runs until
    /// [`ServiceCollector::set_paused`] resumes it.
    pub fn start(refresh_rate: Duration, paused: bool) -> io::Result<Self> {
        let (snapshot_tx, receiver) = latest_snapshot::channel();
        let control = Arc::new(CollectorControl::new_paused(paused));
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("systemd-services".into())
            .spawn(move || {
                run_collector(
                    refresh_rate,
                    &worker_control,
                    || collect_services(&worker_control),
                    |snapshot| snapshot_tx.publish(snapshot),
                );
            })?;

        Ok(Self {
            receiver,
            control,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<ServiceSnapshot> {
        self.receiver.take_latest()
    }

    pub fn request_refresh(&self, generation: ServiceRefreshGeneration) {
        self.control.request_refresh(generation);
    }

    pub fn set_paused(&self, paused: bool) {
        self.control.set_paused(paused);
    }
}

impl Drop for ServiceCollector {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_collector(
    refresh_rate: Duration,
    control: &CollectorControl,
    mut collect: impl FnMut() -> ServiceSnapshot,
    mut publish: impl FnMut(ServiceSnapshot) -> bool,
) {
    let mut refresh_progress = ServiceRefreshProgress::default();
    // Returns immediately unless the collector starts paused.
    let mut collection_generation = match control.wait(Duration::ZERO) {
        CollectorWake::Refresh(generation) => Some(generation),
        CollectorWake::Timeout => None,
        CollectorWake::Stop => return,
    };

    loop {
        let snapshot = refresh_progress.finish_collection(collect(), collection_generation);
        if !publish(snapshot) {
            break;
        }

        collection_generation = match control.wait(refresh_rate) {
            CollectorWake::Refresh(generation) => Some(generation),
            CollectorWake::Timeout => None,
            CollectorWake::Stop => break,
        };
    }
}

fn collect_services(control: &CollectorControl) -> ServiceSnapshot {
    let mut command = Command::new("systemctl");
    command
        .args([
            "--system",
            "--no-pager",
            "--no-ask-password",
            "--all",
            "--type=service",
            "--property=Id,LoadState,ActiveState,SubState,Description",
            "show",
            "*.service",
        ])
        .env("SYSTEMD_COLORS", "0")
        .env("LC_ALL", "C");

    let output = match run_with_deadline(command, SYSTEMCTL_TIMEOUT, control) {
        Ok(output) => output,
        Err(CommandError::Spawn(error)) => {
            return ServiceSnapshot::error(format!("cannot run systemctl: {error}"))
        }
        Err(CommandError::TimedOut) => {
            return ServiceSnapshot::error("systemctl timed out".to_owned())
        }
        Err(CommandError::Stopped) => {
            return ServiceSnapshot::error("systemctl collection stopped".to_owned())
        }
    };

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.lines().next().unwrap_or("systemctl failed").trim();
        return ServiceSnapshot::error(message.to_owned());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut services = parse_systemctl_show(&text);
    services.sort_by(|left, right| left.unit.cmp(&right.unit));
    services.dedup_by(|left, right| left.unit == right.unit);

    ServiceSnapshot {
        services,
        error: None,
        completed_refresh_generation: 0,
    }
}

#[derive(Debug)]
enum CommandError {
    Spawn(io::Error),
    TimedOut,
    Stopped,
}

/// Runs `command` to completion, killing it on timeout or collector stop.
///
/// Both pipes are drained on helper threads so a large listing cannot fill a pipe
/// buffer and deadlock the child while this thread waits for it to exit.
fn run_with_deadline(
    mut command: Command,
    timeout: Duration,
    control: &CollectorControl,
) -> Result<Output, CommandError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(CommandError::Spawn)?;
    let stdout = drain_pipe(child.stdout.take(), "systemctl-stdout");
    let stderr = drain_pipe(child.stderr.take(), "systemctl-stderr");
    let (stdout, stderr) = match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (stdout, stderr) => {
            kill_and_reap(&mut child);
            let error = [stdout.err(), stderr.err()].into_iter().flatten().next();
            return Err(CommandError::Spawn(error.unwrap_or_else(|| {
                io::Error::other("cannot read systemctl output")
            })));
        }
    };

    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(CommandError::Spawn(error)),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(CommandError::TimedOut);
        }
        if control.stopped_within(remaining.min(COMMAND_POLL_INTERVAL)) {
            break Err(CommandError::Stopped);
        }
    };

    if outcome.is_err() {
        kill_and_reap(&mut child);
    }
    // The child has exited or been reaped, so both pipe writers are closed and the
    // readers finish promptly.
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    outcome.map(|status| Output {
        status,
        stdout,
        stderr,
    })
}

fn drain_pipe(
    pipe: Option<impl Read + Send + 'static>,
    name: &str,
) -> io::Result<JoinHandle<Vec<u8>>> {
    thread::Builder::new().name(name.into()).spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    })
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

impl ServiceSnapshot {
    fn error(message: String) -> Self {
        Self {
            services: Vec::new(),
            error: Some(message),
            completed_refresh_generation: 0,
        }
    }
}

fn parse_systemctl_show(contents: &str) -> Vec<ServiceInfo> {
    let mut services = Vec::new();
    let mut current = ServiceInfo::default();

    for line in contents.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            let record = std::mem::take(&mut current);
            if !record.unit.is_empty() {
                services.push(record);
            }
            continue;
        }

        let Some((property, value)) = line.split_once('=') else {
            continue;
        };
        match property {
            "Id" => current.unit = value.to_owned(),
            "LoadState" => current.load_state = value.to_owned(),
            "ActiveState" => current.active_state = value.to_owned(),
            "SubState" => current.sub_state = value.to_owned(),
            "Description" => current.description = value.to_owned(),
            _ => {}
        }
    }

    services
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_requested_during_collection_requires_the_follow_up_collection() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let mut collection = 0;
            run_collector(
                Duration::from_secs(60),
                &worker_control,
                || {
                    started_tx.send(collection).unwrap();
                    release_rx.recv().unwrap();
                    collection += 1;
                    ServiceSnapshot::default()
                },
                |snapshot| snapshot_tx.send(snapshot).is_ok(),
            );
        });

        assert_eq!(started_rx.recv().unwrap(), 0);
        control.request_refresh(7);
        release_tx.send(()).unwrap();
        assert_eq!(snapshot_rx.recv().unwrap().completed_refresh_generation, 0);

        assert_eq!(started_rx.recv().unwrap(), 1);
        release_tx.send(()).unwrap();
        assert_eq!(snapshot_rx.recv().unwrap().completed_refresh_generation, 7);

        control.stop();
        worker.join().unwrap();
    }

    #[test]
    fn paused_collector_runs_no_command_until_resumed_with_one_refresh() {
        let control = Arc::new(CollectorControl::new_paused(true));
        let worker_control = Arc::clone(&control);
        let collections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_collections = Arc::clone(&collections);
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            run_collector(
                Duration::from_secs(3600),
                &worker_control,
                || {
                    worker_collections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ServiceSnapshot::default()
                },
                |snapshot| snapshot_tx.send(snapshot).is_ok(),
            );
        });

        thread::sleep(Duration::from_millis(100));
        assert_eq!(collections.load(std::sync::atomic::Ordering::SeqCst), 0);

        control.request_refresh(1);
        control.set_paused(false);
        let snapshot = snapshot_rx.recv().unwrap();
        assert_eq!(snapshot.completed_refresh_generation, 1);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            collections.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "resume plus refresh must collect exactly once"
        );

        control.set_paused(true);
        control.stop();
        worker.join().unwrap();
    }

    #[test]
    fn completed_generation_survives_latest_snapshot_replacement() {
        let mut progress = ServiceRefreshProgress::default();
        let completed = progress.finish_collection(ServiceSnapshot::default(), Some(11));
        let periodic = progress.finish_collection(ServiceSnapshot::default(), None);
        let (publisher, receiver) = latest_snapshot::channel();

        assert!(publisher.publish(completed));
        assert!(publisher.publish(periodic));

        assert_eq!(
            receiver.take_latest().unwrap().completed_refresh_generation,
            11
        );
    }

    #[test]
    fn periodic_collections_do_not_invent_refresh_completion() {
        let mut progress = ServiceRefreshProgress::default();

        let first = progress.finish_collection(ServiceSnapshot::default(), None);
        let second = progress.finish_collection(ServiceSnapshot::default(), None);

        assert_eq!(first.completed_refresh_generation, 0);
        assert_eq!(second.completed_refresh_generation, 0);
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn hung_command_times_out_and_is_killed() {
        let control = CollectorControl::default();
        let started = Instant::now();

        let result = run_with_deadline(shell("sleep 30"), Duration::from_millis(200), &control);

        assert!(matches!(result, Err(CommandError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stop_during_collection_aborts_the_command_promptly() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let started = Instant::now();
        let worker = thread::spawn(move || {
            run_with_deadline(shell("sleep 30"), Duration::from_secs(60), &worker_control)
        });

        thread::sleep(Duration::from_millis(100));
        control.stop();

        assert!(matches!(worker.join().unwrap(), Err(CommandError::Stopped)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        let control = CollectorControl::default();

        let output = run_with_deadline(
            shell(
                "i=0; while [ $i -lt 20000 ]; do echo line-$i; echo err-$i >&2; i=$((i+1)); done",
            ),
            Duration::from_secs(30),
            &control,
        )
        .unwrap();

        assert!(output.status.success());
        assert!(output.stdout.len() > 128 * 1024);
        assert!(output.stderr.len() > 64 * 1024);
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("line-19999\n"));
    }

    #[test]
    fn failing_command_reports_status_and_stderr() {
        let control = CollectorControl::default();

        let output = run_with_deadline(
            shell("echo boom >&2; exit 3"),
            Duration::from_secs(10),
            &control,
        )
        .unwrap();

        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stderr, b"boom\n");
    }

    #[test]
    fn missing_program_is_a_spawn_error() {
        let control = CollectorControl::default();

        let result = run_with_deadline(
            Command::new("/nonexistent/tuxctl-systemctl"),
            Duration::from_secs(1),
            &control,
        );

        assert!(matches!(result, Err(CommandError::Spawn(_))));
    }

    #[test]
    fn collector_stop_during_a_hung_collection_joins_promptly() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            run_collector(
                Duration::from_secs(60),
                &worker_control,
                || match run_with_deadline(shell("sleep 30"), SYSTEMCTL_TIMEOUT, &worker_control) {
                    Ok(_) => ServiceSnapshot::default(),
                    Err(_) => ServiceSnapshot::error("stopped".into()),
                },
                |snapshot| snapshot_tx.send(snapshot).is_ok(),
            );
        });

        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        control.stop();
        worker.join().unwrap();

        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(snapshot_rx.recv().unwrap().error.is_some());
    }

    #[test]
    fn parses_machine_readable_systemctl_properties() {
        let output = "\
Id=alpha.service
LoadState=loaded
ActiveState=active
SubState=running
Description=Alpha worker service

Id=beta.service
LoadState=not-found
ActiveState=inactive
SubState=dead
Description=
";

        assert_eq!(
            parse_systemctl_show(output),
            vec![
                ServiceInfo {
                    unit: "alpha.service".into(),
                    load_state: "loaded".into(),
                    active_state: "active".into(),
                    sub_state: "running".into(),
                    description: "Alpha worker service".into(),
                },
                ServiceInfo {
                    unit: "beta.service".into(),
                    load_state: "not-found".into(),
                    active_state: "inactive".into(),
                    sub_state: "dead".into(),
                    description: String::new(),
                },
            ]
        );
    }

    #[test]
    fn skips_incomplete_records_without_unit_identity() {
        let output = "ActiveState=active\nDescription=no identity\n\nId=real.service\n";

        assert_eq!(
            parse_systemctl_show(output),
            vec![ServiceInfo {
                unit: "real.service".into(),
                ..ServiceInfo::default()
            }]
        );
    }

    #[test]
    fn property_values_may_contain_equals_signs() {
        let output = "Id=query.service\nDescription=Worker with A=B\n";

        assert_eq!(
            parse_systemctl_show(output)[0].description,
            "Worker with A=B"
        );
    }
}
