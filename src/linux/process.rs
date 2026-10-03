//! Processes: the `/proc/<pid>/stat` scanner (CPU from tick deltas, memory,
//! state, kernel threads), command lines cached per process identity and read
//! again when the process name changes (after an exec), and signal delivery. A
//! process is identified by `(PID, start time)`; a signal is sent through a
//! pidfd only after that identity is checked again, and never by PID alone.

use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use super::{
    control::{run_periodic, CollectorControl},
    latest_snapshot::{self, LatestReceiver},
};

const PROC: &str = "/proc";
/// `PF_KTHREAD` in the `flags` field of `/proc/<pid>/stat`.
const PF_KTHREAD: u64 = 0x0020_0000;

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
    /// A kernel thread (no user-space program); false when unknown.
    pub kernel_thread: bool,
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
    Unsupported,
    Failed(String),
}

impl std::fmt::Display for ProcessSignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessNotFound => write!(f, "process already exited"),
            Self::StaleIdentity => write!(f, "process identity changed (PID reused)"),
            Self::PermissionDenied => write!(f, "permission denied"),
            Self::Unsupported => write!(f, "pidfd signaling is not supported"),
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

pub fn verify_and_send_signal_at<H, O, S>(
    proc_dir: &Path,
    identity: ProcessIdentity,
    signal: ProcessSignal,
    open_pidfd: O,
    send_signal: S,
) -> Result<(), ProcessSignalError>
where
    O: FnOnce(libc::pid_t) -> io::Result<H>,
    S: FnOnce(&H, libc::c_int) -> io::Result<()>,
{
    if identity.pid == 0 {
        return Err(ProcessSignalError::Failed("cannot signal PID 0".into()));
    }

    let pid = libc::pid_t::try_from(identity.pid)
        .map_err(|_| ProcessSignalError::Failed(format!("invalid PID {}", identity.pid)))?;
    let pidfd = open_pidfd(pid).map_err(map_signal_error)?;

    let stat_path = proc_dir.join(identity.pid.to_string()).join("stat");
    let stat_contents = match read_stat(&stat_path) {
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

    send_signal(&pidfd, signal.as_c_int()).map_err(map_signal_error)
}

fn map_signal_error(error: io::Error) -> ProcessSignalError {
    match error.raw_os_error() {
        Some(libc::ESRCH) => ProcessSignalError::ProcessNotFound,
        Some(libc::EPERM) | Some(libc::EACCES) => ProcessSignalError::PermissionDenied,
        Some(libc::ENOSYS) => ProcessSignalError::Unsupported,
        _ => ProcessSignalError::Failed(error.to_string()),
    }
}

fn pidfd_open(pid: libc::pid_t) -> io::Result<OwnedFd> {
    // SAFETY: `SYS_pidfd_open` accepts a PID value and zero flags. On success it
    // returns a new file descriptor, which is immediately transferred to `OwnedFd`.
    let result = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: A successful `pidfd_open` result is a newly owned nonnegative
        // file descriptor. `OwnedFd` closes it on every subsequent exit path.
        Ok(unsafe { OwnedFd::from_raw_fd(result as libc::c_int) })
    }
}

fn pidfd_send_signal(pidfd: &OwnedFd, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: `pidfd` is live for the duration of the call, `signal` is one of
    // the fixed supported signals, and null siginfo with zero flags is valid.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0_u32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn send_process_signal(
    identity: ProcessIdentity,
    signal: ProcessSignal,
) -> Result<(), ProcessSignalError> {
    verify_and_send_signal_at(
        Path::new(PROC),
        identity,
        signal,
        pidfd_open,
        pidfd_send_signal,
    )
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
    control: Arc<CollectorControl>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (snapshot_tx, receiver) = latest_snapshot::channel();
        let control = Arc::new(CollectorControl::new(refresh_rate, false));
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("process-metrics".into())
            .spawn(move || {
                let mut sampler = ProcessSampler::default();

                run_periodic(
                    &worker_control,
                    || sampler.collect(),
                    |snapshot| snapshot_tx.publish(snapshot),
                );
            })?;

        Ok(Self {
            receiver,
            control,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<ProcessSnapshot> {
        self.receiver.take_latest()
    }

    /// Applies a new sampling period to the running worker.
    pub fn set_period(&self, period: Duration) {
        self.control.set_period(period);
    }
}

impl Drop for ProcessCollector {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct ProcessSampler {
    previous_system_ticks: Option<u64>,
    previous_process_ticks: HashMap<ProcessIdentity, u64>,
    /// Command lines of live processes, keyed by identity and tagged with the
    /// `comm` name they were read under. Rebuilt every collection, so it only
    /// holds processes present in the latest snapshot.
    commands: HashMap<ProcessIdentity, CachedCommand>,
}

struct CachedCommand {
    name: String,
    command: Option<String>,
}

impl ProcessSampler {
    fn collect(&mut self) -> ProcessSnapshot {
        self.collect_at(Path::new(PROC))
    }

    fn collect_at(&mut self, proc_root: &Path) -> ProcessSnapshot {
        let system_cpu = fs::read_to_string(proc_root.join("stat"))
            .ok()
            .and_then(|contents| parse_system_cpu(&contents));
        let page_size = page_size();
        let entries = match fs::read_dir(proc_root) {
            Ok(entries) => entries,
            Err(error) => {
                return ProcessSnapshot {
                    processes: Vec::new(),
                    error: Some(error.to_string()),
                };
            }
        };

        let mut raw_processes = Vec::new();
        let mut next_commands = HashMap::with_capacity(self.commands.len());
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };

            let path = entry.path();
            if let Some(mut process) = read_process(&path, pid, page_size) {
                let identity = ProcessIdentity {
                    pid: process.pid,
                    start_time: process.start_time,
                };
                // Only new identities read cmdline/exe. A changed comm means the
                // process exec'd since it was cached, so its command is re-read.
                let cached = self
                    .commands
                    .remove(&identity)
                    .filter(|cached| cached.name == process.name)
                    .unwrap_or_else(|| CachedCommand {
                        name: process.name.clone(),
                        command: read_command(&path),
                    });
                process.command = cached.command.clone();
                next_commands.insert(identity, cached);
                raw_processes.push(process);
            }
        }
        self.commands = next_commands;

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
                kernel_thread: process.kernel_thread,
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
    kernel_thread: bool,
}

/// Reads `/proc/<pid>/stat`. The `comm` field is whatever bytes the process
/// set (`prctl(PR_SET_NAME)`), not necessarily UTF-8; a strict read would drop
/// such a process from the list and make it unsignalable.
fn read_stat(path: &Path) -> io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn read_process(path: &Path, expected_pid: u32, page_size: u64) -> Option<RawProcess> {
    let contents = read_stat(&path.join("stat")).ok()?;
    let process = parse_process_stat(&contents, page_size)?;
    if process.pid != expected_pid {
        return None;
    }

    Some(process)
}

/// The most of a command line that is read: more than any row or the detail
/// view shows, while a process whose owner gave it megabytes of arguments
/// costs no more memory than any other.
const MAX_COMMAND_BYTES: u64 = 4096;

fn read_command(path: &Path) -> Option<String> {
    let command = read_prefix(&path.join("cmdline"), MAX_COMMAND_BYTES)
        .ok()
        .and_then(|(bytes, truncated)| {
            let command = parse_command_line(&bytes)?;
            Some(if truncated {
                format!("{command}…")
            } else {
                command
            })
        });

    command.or_else(|| {
        fs::read_link(path.join("exe"))
            .ok()
            .map(|executable| executable.to_string_lossy().into_owned())
    })
}

/// Up to `limit` bytes of the file at `path`, and whether it had more.
fn read_prefix(path: &Path, limit: u64) -> io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, truncated))
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
    // Unparsable flags only lose the kernel-thread hint; the process is kept.
    let kernel_thread = fields[6]
        .parse::<u64>()
        .is_ok_and(|flags| flags & PF_KTHREAD != 0);
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
        kernel_thread,
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
mod tests;
