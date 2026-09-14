use std::{
    collections::HashMap,
    fs, io,
    path::Path,
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use super::latest_snapshot::{self, LatestReceiver};

const PROC: &str = "/proc";

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: u64,
    pub command: Option<String>,
    pub state: String,
    pub parent_pid: u32,
    pub(crate) state_code: char,
    pub(crate) start_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSignal {
    Term,
    Kill,
}

impl ProcessSignal {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM (15)",
            Self::Kill => "SIGKILL (9)",
        }
    }

    pub const fn as_c_int(self) -> libc::c_int {
        match self {
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessSignalError {
    ProcessNotFound,
    StaleIdentity,
    PermissionDenied,
    Failed(String),
}

impl std::fmt::Display for ProcessSignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessNotFound => write!(f, "process already exited"),
            Self::StaleIdentity => write!(f, "process identity changed (PID reused)"),
            Self::PermissionDenied => write!(f, "permission denied"),
            Self::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProcessSignalError {}

pub fn read_process_start_time(stat_contents: &str) -> Option<u64> {
    let name_start = stat_contents.find('(')?;
    let name_end = stat_contents.rfind(')')?;
    if name_end <= name_start {
        return None;
    }
    let fields = stat_contents[name_end + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.len() <= 19 {
        return None;
    }
    fields[19].parse::<u64>().ok()
}

pub fn verify_and_send_signal_at<F>(
    proc_dir: &Path,
    identity: ProcessIdentity,
    signal: ProcessSignal,
    kill_fn: F,
) -> Result<(), ProcessSignalError>
where
    F: FnOnce(libc::pid_t, libc::c_int) -> io::Result<()>,
{
    if identity.pid == 0 {
        return Err(ProcessSignalError::Failed("cannot signal PID 0".into()));
    }

    let stat_path = proc_dir.join(identity.pid.to_string()).join("stat");
    let stat_contents = match fs::read_to_string(&stat_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ProcessSignalError::ProcessNotFound);
        }
        Err(err) => {
            return Err(ProcessSignalError::Failed(err.to_string()));
        }
    };

    let Some(current_start_time) = read_process_start_time(&stat_contents) else {
        return Err(ProcessSignalError::ProcessNotFound);
    };

    if current_start_time != identity.start_time {
        return Err(ProcessSignalError::StaleIdentity);
    }

    match kill_fn(identity.pid as libc::pid_t, signal.as_c_int()) {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.raw_os_error() == Some(libc::ESRCH) {
                Err(ProcessSignalError::ProcessNotFound)
            } else if err.raw_os_error() == Some(libc::EPERM) {
                Err(ProcessSignalError::PermissionDenied)
            } else {
                Err(ProcessSignalError::Failed(err.to_string()))
            }
        }
    }
}

pub fn send_process_signal(
    identity: ProcessIdentity,
    signal: ProcessSignal,
) -> Result<(), ProcessSignalError> {
    verify_and_send_signal_at(Path::new(PROC), identity, signal, |pid, sig| {
        let res = unsafe { libc::kill(pid, sig) };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

impl ProcessInfo {
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            pid: self.pid,
            start_time: self.start_time,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessSnapshot {
    pub processes: Vec<ProcessInfo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessSummary {
    pub total: usize,
    pub running: usize,
    pub zombies: usize,
}

impl ProcessSnapshot {
    pub fn summary(&self) -> ProcessSummary {
        ProcessSummary {
            total: self.processes.len(),
            running: self
                .processes
                .iter()
                .filter(|process| process.state_code == 'R')
                .count(),
            zombies: self
                .processes
                .iter()
                .filter(|process| process.state_code == 'Z')
                .count(),
        }
    }
}

pub struct ProcessCollector {
    receiver: LatestReceiver<ProcessSnapshot>,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (snapshot_tx, receiver) = latest_snapshot::channel();
        let (stop, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("process-metrics".into())
            .spawn(move || {
                let mut sampler = ProcessSampler::default();

                loop {
                    if !snapshot_tx.publish(sampler.collect()) {
                        break;
                    }

                    match stop_rx.recv_timeout(refresh_rate) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;

        Ok(Self {
            receiver,
            stop,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<ProcessSnapshot> {
        self.receiver.take_latest()
    }
}

impl Drop for ProcessCollector {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct ProcessSampler {
    previous_system_ticks: Option<u64>,
    previous_process_ticks: HashMap<ProcessIdentity, u64>,
}

impl ProcessSampler {
    fn collect(&mut self) -> ProcessSnapshot {
        let system_cpu = fs::read_to_string(Path::new(PROC).join("stat"))
            .ok()
            .and_then(|contents| parse_system_cpu(&contents));
        let page_size = page_size();
        let entries = match fs::read_dir(PROC) {
            Ok(entries) => entries,
            Err(error) => {
                return ProcessSnapshot {
                    processes: Vec::new(),
                    error: Some(error.to_string()),
                };
            }
        };

        let mut raw_processes = Vec::new();
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };

            if let Some(process) = read_process(&entry.path(), pid, page_size) {
                raw_processes.push(process);
            }
        }

        let system_delta = system_cpu.zip(self.previous_system_ticks).and_then(
            |((current, cpu_count), previous)| {
                current
                    .checked_sub(previous)
                    .map(|delta| (delta, cpu_count))
            },
        );
        let mut next_process_ticks = HashMap::with_capacity(raw_processes.len());
        let mut processes = Vec::with_capacity(raw_processes.len());

        for process in raw_processes {
            let identity = ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            };
            let cpu_percent = system_delta.and_then(|(total_delta, cpu_count)| {
                let previous = self.previous_process_ticks.get(&identity)?;
                process_cpu_percent(*previous, process.cpu_ticks, total_delta, cpu_count)
            });
            next_process_ticks.insert(identity, process.cpu_ticks);
            processes.push(ProcessInfo {
                pid: process.pid,
                name: process.name,
                cpu_percent,
                memory_bytes: process.memory_bytes,
                command: process.command,
                state: process_state(process.state).into(),
                parent_pid: process.parent_pid,
                state_code: process.state,
                start_time: process.start_time,
            });
        }

        if let Some((total, _)) = system_cpu {
            self.previous_system_ticks = Some(total);
            self.previous_process_ticks = next_process_ticks;
        }

        ProcessSnapshot {
            processes,
            error: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RawProcess {
    pid: u32,
    name: String,
    state: char,
    parent_pid: u32,
    cpu_ticks: u64,
    start_time: u64,
    memory_bytes: u64,
    command: Option<String>,
}

fn read_process(path: &Path, expected_pid: u32, page_size: u64) -> Option<RawProcess> {
    let contents = fs::read_to_string(path.join("stat")).ok()?;
    let mut process = parse_process_stat(&contents, page_size)?;
    if process.pid != expected_pid {
        return None;
    }

    process.command = read_command(path);
    Some(process)
}

fn read_command(path: &Path) -> Option<String> {
    let command = fs::read(path.join("cmdline"))
        .ok()
        .and_then(|bytes| parse_command_line(&bytes));

    command.or_else(|| {
        fs::read_link(path.join("exe"))
            .ok()
            .map(|executable| executable.to_string_lossy().into_owned())
    })
}

fn parse_command_line(bytes: &[u8]) -> Option<String> {
    let arguments = bytes
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument))
        .collect::<Vec<_>>()
        .join(" ");
    (!arguments.is_empty()).then_some(arguments)
}

fn parse_process_stat(contents: &str, page_size: u64) -> Option<RawProcess> {
    let name_start = contents.find('(')?;
    let name_end = contents.rfind(')')?;
    if name_end <= name_start {
        return None;
    }

    let pid = contents[..name_start].trim().parse::<u32>().ok()?;
    let name = contents[name_start + 1..name_end].to_owned();
    let fields = contents[name_end + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.len() <= 21 {
        return None;
    }

    let state = fields[0].chars().next()?;
    let parent_pid = fields[1].parse::<u32>().ok()?;
    let user_ticks = fields[11].parse::<u64>().ok()?;
    let system_ticks = fields[12].parse::<u64>().ok()?;
    let start_time = fields[19].parse::<u64>().ok()?;
    let resident_pages = fields[21].parse::<i64>().ok()?;
    let memory_bytes = u64::try_from(resident_pages.max(0))
        .unwrap_or(0)
        .saturating_mul(page_size);

    Some(RawProcess {
        pid,
        name,
        state,
        parent_pid,
        cpu_ticks: user_ticks.checked_add(system_ticks)?,
        start_time,
        memory_bytes,
        command: None,
    })
}

fn parse_system_cpu(contents: &str) -> Option<(u64, usize)> {
    let mut lines = contents.lines();
    let aggregate = lines.next()?;
    let mut fields = aggregate.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let total = fields
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .into_iter()
        .try_fold(0_u64, u64::checked_add)?;
    let cpu_count = lines
        .filter(|line| {
            line.strip_prefix("cpu")
                .and_then(|suffix| suffix.split_whitespace().next())
                .is_some_and(|suffix| suffix.chars().all(|character| character.is_ascii_digit()))
        })
        .count()
        .max(1);

    Some((total, cpu_count))
}

fn process_cpu_percent(
    previous_ticks: u64,
    current_ticks: u64,
    system_delta: u64,
    cpu_count: usize,
) -> Option<f64> {
    if system_delta == 0 {
        return None;
    }

    let process_delta = current_ticks.checked_sub(previous_ticks)?;
    Some((process_delta as f64 / system_delta as f64) * cpu_count as f64 * 100.0)
}

fn process_state(state: char) -> &'static str {
    match state {
        'R' => "R (running)",
        'S' => "S (sleeping)",
        'D' => "D (disk sleep)",
        'Z' => "Z (zombie)",
        'T' | 't' => "T (stopped)",
        'I' => "I (idle)",
        'X' | 'x' => "X (dead)",
        _ => "? (unknown)",
    }
}

fn page_size() -> u64 {
    // SAFETY: `sysconf` has no pointer arguments and `_SC_PAGESIZE` is a valid query.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(size)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(4096)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat_line(name: &str) -> String {
        let fields = [
            "S", "7", "0", "0", "0", "0", "0", "0", "0", "0", "0", "120", "30", "0", "0", "0", "0",
            "0", "0", "900", "0", "25",
        ];
        format!("42 ({name}) {}", fields.join(" "))
    }

    #[test]
    fn parses_process_stat_with_spaces_and_parentheses_in_name() {
        let process = parse_process_stat(&stat_line("worker (pool)"), 4096).unwrap();

        assert_eq!(process.pid, 42);
        assert_eq!(process.name, "worker (pool)");
        assert_eq!(process.state, 'S');
        assert_eq!(process.parent_pid, 7);
        assert_eq!(process.cpu_ticks, 150);
        assert_eq!(process.start_time, 900);
        assert_eq!(process.memory_bytes, 25 * 4096);
    }

    #[test]
    fn calculates_process_cpu_from_two_intervals() {
        assert_eq!(process_cpu_percent(100, 125, 200, 4), Some(50.0));
        assert_eq!(process_cpu_percent(100, 125, 0, 4), None);
        assert_eq!(process_cpu_percent(125, 100, 200, 4), None);
    }

    #[test]
    fn derives_process_summary_from_collected_state_codes() {
        let process = |pid, state_code| ProcessInfo {
            pid,
            name: format!("process-{pid}"),
            cpu_percent: None,
            memory_bytes: 0,
            command: None,
            state: process_state(state_code).into(),
            parent_pid: 1,
            state_code,
            start_time: u64::from(pid),
        };
        let snapshot = ProcessSnapshot {
            processes: vec![
                process(1, 'R'),
                process(2, 'S'),
                process(3, 'R'),
                process(4, 'Z'),
            ],
            error: None,
        };

        assert_eq!(
            snapshot.summary(),
            ProcessSummary {
                total: 4,
                running: 2,
                zombies: 1,
            }
        );
    }

    #[test]
    fn parses_system_cpu_total_and_count() {
        let contents = "cpu  10 20 30 40 50 60 70 80 90 100\ncpu0 1 2\ncpu1 3 4\nintr 0\n";

        assert_eq!(parse_system_cpu(contents), Some((360, 2)));
    }

    #[test]
    fn parses_nul_separated_command_line() {
        assert_eq!(parse_command_line(b""), None);
        assert_eq!(parse_command_line(b"\0"), None);
        assert_eq!(
            parse_command_line(b"/usr/bin/demo\0--flag\0"),
            Some("/usr/bin/demo --flag".into())
        );
    }

    #[test]
    fn parses_process_start_time_from_stat() {
        let stat =
            "123 (my process) S 1 123 123 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 777 1000 200";
        assert_eq!(read_process_start_time(stat), Some(777));
    }

    #[test]
    fn signal_verification_succeeds_with_matching_identity() {
        let unique = format!(
            "tuxctl_test_sig_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let temp_dir = std::env::temp_dir().join(unique);
        let pid_dir = temp_dir.join("123");
        fs::create_dir_all(&pid_dir).unwrap();
        let stat = "123 (test) S 1 123 123 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 500 1000 200";
        fs::write(pid_dir.join("stat"), stat).unwrap();

        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut called_sig = None;
        let res =
            verify_and_send_signal_at(&temp_dir, identity, ProcessSignal::Term, |pid, sig| {
                called_sig = Some((pid, sig));
                Ok(())
            });

        assert_eq!(res, Ok(()));
        assert_eq!(called_sig, Some((123, libc::SIGTERM)));
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn signal_verification_rejects_stale_identity_without_sending_signal() {
        let unique = format!(
            "tuxctl_test_stale_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let temp_dir = std::env::temp_dir().join(unique);
        let pid_dir = temp_dir.join("123");
        fs::create_dir_all(&pid_dir).unwrap();
        // Start time in stat is 999, but identity has start_time 500
        let stat =
            "123 (new_proc) S 1 123 123 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 999 1000 200";
        fs::write(pid_dir.join("stat"), stat).unwrap();

        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut called = false;
        let res =
            verify_and_send_signal_at(&temp_dir, identity, ProcessSignal::Kill, |_pid, _sig| {
                called = true;
                Ok(())
            });

        assert_eq!(res, Err(ProcessSignalError::StaleIdentity));
        assert!(!called, "Signal must NEVER be sent to a stale PID identity");
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn signal_verification_reports_missing_process_without_sending_signal() {
        let unique = format!(
            "tuxctl_test_missing_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let temp_dir = std::env::temp_dir().join(unique);
        let _ = fs::create_dir_all(&temp_dir);

        let identity = ProcessIdentity {
            pid: 99999,
            start_time: 500,
        };
        let mut called = false;
        let res =
            verify_and_send_signal_at(&temp_dir, identity, ProcessSignal::Term, |_pid, _sig| {
                called = true;
                Ok(())
            });

        assert_eq!(res, Err(ProcessSignalError::ProcessNotFound));
        assert!(!called);
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn signal_verification_handles_permission_denied() {
        let unique = format!(
            "tuxctl_test_eperm_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let temp_dir = std::env::temp_dir().join(unique);
        let pid_dir = temp_dir.join("1");
        fs::create_dir_all(&pid_dir).unwrap();
        let stat = "1 (systemd) S 0 1 1 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 1 1000 200";
        fs::write(pid_dir.join("stat"), stat).unwrap();

        let identity = ProcessIdentity {
            pid: 1,
            start_time: 1,
        };
        let res =
            verify_and_send_signal_at(&temp_dir, identity, ProcessSignal::Kill, |_pid, _sig| {
                Err(io::Error::from_raw_os_error(libc::EPERM))
            });

        assert_eq!(res, Err(ProcessSignalError::PermissionDenied));
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn signal_verification_rejects_pid_zero() {
        let temp_dir = std::env::temp_dir();
        let identity = ProcessIdentity {
            pid: 0,
            start_time: 0,
        };
        let mut called = false;
        let res =
            verify_and_send_signal_at(&temp_dir, identity, ProcessSignal::Term, |_pid, _sig| {
                called = true;
                Ok(())
            });

        assert_eq!(
            res,
            Err(ProcessSignalError::Failed("cannot signal PID 0".into()))
        );
        assert!(!called);
    }

    #[test]
    fn parses_process_stat_with_negative_resident_pages() {
        let stat = "123 (test) S 1 123 123 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 500 1000 -5";
        let proc = parse_process_stat(stat, 4096).expect("process stat should parse successfully");
        assert_eq!(proc.pid, 123);
        assert_eq!(proc.memory_bytes, 0);
    }
}
