use std::{
    collections::HashMap,
    fs, io,
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
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("process-metrics".into())
            .spawn(move || {
                let mut sampler = ProcessSampler::default();

                run_periodic(
                    refresh_rate,
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
    let process = parse_process_stat(&contents, page_size)?;
    if process.pid != expected_pid {
        return None;
    }

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
    use std::{
        cell::Cell,
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    fn stat_line(name: &str) -> String {
        let fields = [
            "S", "7", "0", "0", "0", "0", "0", "0", "0", "0", "0", "120", "30", "0", "0", "0", "0",
            "0", "0", "900", "0", "25",
        ];
        format!("42 ({name}) {}", fields.join(" "))
    }

    fn temp_proc_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tuxctl_process_{label}_{}_{}",
            std::process::id(),
            NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn write_process_stat(proc_dir: &Path, pid: u32, name: &str, start_time: u64) {
        let pid_dir = proc_dir.join(pid.to_string());
        fs::create_dir_all(&pid_dir).unwrap();
        let stat = format!(
            "{pid} ({name}) S 1 {pid} {pid} 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 {start_time} 1000 200"
        );
        fs::write(pid_dir.join("stat"), stat).unwrap();
    }

    struct FakePidFd(Rc<Cell<usize>>);

    impl Drop for FakePidFd {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    /// Removes a temporary proc tree when a test ends, including on assertion failure.
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_cmdline(proc_dir: &Path, pid: u32, cmdline: &[u8]) {
        fs::write(proc_dir.join(pid.to_string()).join("cmdline"), cmdline).unwrap();
    }

    fn collected_command(
        sampler: &mut ProcessSampler,
        proc_dir: &Path,
        pid: u32,
    ) -> Option<String> {
        sampler
            .collect_at(proc_dir)
            .processes
            .into_iter()
            .find(|process| process.pid == pid)
            .expect("process collected")
            .command
    }

    #[test]
    fn command_line_is_read_once_per_process_identity() {
        let proc_dir = temp_proc_dir("cmdline_cache");
        let _cleanup = TempDir(proc_dir.clone());
        write_process_stat(&proc_dir, 10, "server", 500);
        write_cmdline(&proc_dir, 10, b"/usr/bin/server\0--flag\0");
        let mut sampler = ProcessSampler::default();

        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 10).as_deref(),
            Some("/usr/bin/server --flag")
        );
        write_cmdline(&proc_dir, 10, b"rewritten\0");
        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 10).as_deref(),
            Some("/usr/bin/server --flag"),
            "an unchanged identity must reuse the cached command"
        );
    }

    #[test]
    fn reused_pid_with_new_start_time_rereads_the_command() {
        let proc_dir = temp_proc_dir("cmdline_pid_reuse");
        let _cleanup = TempDir(proc_dir.clone());
        write_process_stat(&proc_dir, 10, "server", 500);
        write_cmdline(&proc_dir, 10, b"old\0");
        let mut sampler = ProcessSampler::default();
        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 10).as_deref(),
            Some("old")
        );

        write_process_stat(&proc_dir, 10, "server", 501);
        write_cmdline(&proc_dir, 10, b"new\0");

        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 10).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn exec_detected_by_comm_change_rereads_the_command() {
        let proc_dir = temp_proc_dir("cmdline_exec");
        let _cleanup = TempDir(proc_dir.clone());
        write_process_stat(&proc_dir, 10, "bash", 500);
        write_cmdline(&proc_dir, 10, b"bash\0");
        let mut sampler = ProcessSampler::default();
        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 10).as_deref(),
            Some("bash")
        );

        write_process_stat(&proc_dir, 10, "ls", 500);
        write_cmdline(&proc_dir, 10, b"ls\0-l\0");

        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 10).as_deref(),
            Some("ls -l")
        );
    }

    #[test]
    fn exited_processes_are_evicted_from_the_command_cache() {
        let proc_dir = temp_proc_dir("cmdline_evict");
        let _cleanup = TempDir(proc_dir.clone());
        write_process_stat(&proc_dir, 10, "short", 500);
        write_process_stat(&proc_dir, 11, "long", 600);
        let mut sampler = ProcessSampler::default();
        sampler.collect_at(&proc_dir);
        assert_eq!(sampler.commands.len(), 2);

        fs::remove_dir_all(proc_dir.join("10")).unwrap();
        sampler.collect_at(&proc_dir);

        assert_eq!(sampler.commands.len(), 1);
        assert!(sampler.commands.contains_key(&ProcessIdentity {
            pid: 11,
            start_time: 600
        }));
    }

    #[test]
    fn kernel_threads_without_a_command_are_not_retried() {
        let proc_dir = temp_proc_dir("cmdline_kthread");
        let _cleanup = TempDir(proc_dir.clone());
        write_process_stat(&proc_dir, 2, "kthreadd", 3);
        write_cmdline(&proc_dir, 2, b"");
        let mut sampler = ProcessSampler::default();
        assert_eq!(collected_command(&mut sampler, &proc_dir, 2), None);

        write_cmdline(&proc_dir, 2, b"late\0");

        assert_eq!(
            collected_command(&mut sampler, &proc_dir, 2),
            None,
            "a missing command is cached for the identity instead of re-read every sample"
        );
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
        let temp_dir = temp_proc_dir("success");
        write_process_stat(&temp_dir, 123, "test", 500);

        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut opened_pid = None;
        let mut called_sig = None;
        let res = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |pid| {
                opened_pid = Some(pid);
                Ok(pid)
            },
            |pidfd, sig| {
                called_sig = Some((*pidfd, sig));
                Ok(())
            },
        );

        assert_eq!(res, Ok(()));
        assert_eq!(opened_pid, Some(123));
        assert_eq!(called_sig, Some((123, libc::SIGTERM)));
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn pidfd_is_opened_before_identity_validation_for_both_signals() {
        for (signal, expected_signal) in [
            (ProcessSignal::Term, libc::SIGTERM),
            (ProcessSignal::Kill, libc::SIGKILL),
        ] {
            let temp_dir = temp_proc_dir(signal.name());
            let identity = ProcessIdentity {
                pid: 123,
                start_time: 500,
            };
            let proc_dir_for_open = temp_dir.clone();
            let mut sent_signal = None;

            let result = verify_and_send_signal_at(
                &temp_dir,
                identity,
                signal,
                |pid| {
                    assert_eq!(pid, 123);
                    write_process_stat(&proc_dir_for_open, 123, "test", 500);
                    Ok(())
                },
                |_, signal| {
                    sent_signal = Some(signal);
                    Ok(())
                },
            );

            assert_eq!(result, Ok(()));
            assert_eq!(sent_signal, Some(expected_signal));
            let _ = fs::remove_dir_all(temp_dir);
        }
    }

    #[test]
    fn signal_verification_rejects_stale_identity_without_sending_signal() {
        let temp_dir = temp_proc_dir("stale");
        write_process_stat(&temp_dir, 123, "new_proc", 999);

        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut opened = false;
        let mut signal_called = false;
        let res = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Kill,
            |_| {
                opened = true;
                Ok(())
            },
            |_, _| {
                signal_called = true;
                Ok(())
            },
        );

        assert_eq!(res, Err(ProcessSignalError::StaleIdentity));
        assert!(opened);
        assert!(
            !signal_called,
            "Signal must NEVER be sent to a stale PID identity"
        );
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn simulated_pid_reuse_after_pidfd_open_does_not_signal_replacement() {
        let temp_dir = temp_proc_dir("reused_after_open");
        write_process_stat(&temp_dir, 123, "original", 500);
        let proc_dir_for_open = temp_dir.clone();
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut signal_called = false;

        let result = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Kill,
            |_| {
                write_process_stat(&proc_dir_for_open, 123, "replacement", 999);
                Ok(())
            },
            |_, _| {
                signal_called = true;
                Ok(())
            },
        );

        assert_eq!(result, Err(ProcessSignalError::StaleIdentity));
        assert!(!signal_called);
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn signal_verification_reports_missing_process_without_sending_signal() {
        let temp_dir = temp_proc_dir("missing");
        let _ = fs::create_dir_all(&temp_dir);

        let identity = ProcessIdentity {
            pid: 99999,
            start_time: 500,
        };
        let mut opened = false;
        let mut signal_called = false;
        let res = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |_| {
                opened = true;
                Ok(())
            },
            |_, _| {
                signal_called = true;
                Ok(())
            },
        );

        assert_eq!(res, Err(ProcessSignalError::ProcessNotFound));
        assert!(opened);
        assert!(!signal_called);
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn process_disappearing_after_pidfd_open_does_not_send_signal() {
        let temp_dir = temp_proc_dir("disappears_after_open");
        write_process_stat(&temp_dir, 123, "original", 500);
        let pid_dir_for_open = temp_dir.join("123");
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut signal_called = false;

        let result = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |_| {
                fs::remove_dir_all(&pid_dir_for_open).unwrap();
                Ok(())
            },
            |_, _| {
                signal_called = true;
                Ok(())
            },
        );

        assert_eq!(result, Err(ProcessSignalError::ProcessNotFound));
        assert!(!signal_called);
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn process_missing_at_pidfd_open_fails_without_signaling() {
        let temp_dir = temp_proc_dir("missing_at_open");
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut signal_called = false;

        let result = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |_| Err::<(), _>(io::Error::from_raw_os_error(libc::ESRCH)),
            |_, _| {
                signal_called = true;
                Ok(())
            },
        );

        assert_eq!(result, Err(ProcessSignalError::ProcessNotFound));
        assert!(!signal_called);
    }

    #[test]
    fn signal_verification_handles_permission_denied() {
        let temp_dir = temp_proc_dir("permission");
        write_process_stat(&temp_dir, 1, "systemd", 1);

        let identity = ProcessIdentity {
            pid: 1,
            start_time: 1,
        };
        let res = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Kill,
            |_| Ok(()),
            |_, _| Err(io::Error::from_raw_os_error(libc::EPERM)),
        );

        assert_eq!(res, Err(ProcessSignalError::PermissionDenied));
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn pidfd_send_esrch_is_safe_and_never_retries_by_pid() {
        let temp_dir = temp_proc_dir("send_esrch");
        write_process_stat(&temp_dir, 123, "test", 500);
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut send_attempts = 0;

        let result = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |_| Ok(()),
            |_, _| {
                send_attempts += 1;
                Err(io::Error::from_raw_os_error(libc::ESRCH))
            },
        );

        assert_eq!(result, Err(ProcessSignalError::ProcessNotFound));
        assert_eq!(send_attempts, 1);
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn unavailable_pidfd_syscalls_fail_closed() {
        let temp_dir = temp_proc_dir("unsupported");
        write_process_stat(&temp_dir, 123, "test", 500);
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let mut send_called = false;

        let open_result = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |_| Err::<(), _>(io::Error::from_raw_os_error(libc::ENOSYS)),
            |_, _| {
                send_called = true;
                Ok(())
            },
        );
        assert_eq!(open_result, Err(ProcessSignalError::Unsupported));
        assert!(!send_called);

        let send_result = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Kill,
            |_| Ok(()),
            |_, _| Err(io::Error::from_raw_os_error(libc::ENOSYS)),
        );
        assert_eq!(send_result, Err(ProcessSignalError::Unsupported));
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn pidfd_handle_is_released_on_success_and_every_post_open_error() {
        let temp_dir = temp_proc_dir("pidfd_drop");
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 500,
        };
        let drops = Rc::new(Cell::new(0));

        write_process_stat(&temp_dir, 123, "test", 500);
        assert_eq!(
            verify_and_send_signal_at(
                &temp_dir,
                identity,
                ProcessSignal::Term,
                |_| Ok(FakePidFd(Rc::clone(&drops))),
                |_, _| Ok(())
            ),
            Ok(())
        );
        assert_eq!(drops.get(), 1);

        write_process_stat(&temp_dir, 123, "replacement", 999);
        assert_eq!(
            verify_and_send_signal_at(
                &temp_dir,
                identity,
                ProcessSignal::Kill,
                |_| Ok(FakePidFd(Rc::clone(&drops))),
                |_, _| panic!("stale identity must not be signaled")
            ),
            Err(ProcessSignalError::StaleIdentity)
        );
        assert_eq!(drops.get(), 2);

        fs::remove_dir_all(temp_dir.join("123")).unwrap();
        assert_eq!(
            verify_and_send_signal_at(
                &temp_dir,
                identity,
                ProcessSignal::Term,
                |_| Ok(FakePidFd(Rc::clone(&drops))),
                |_, _| panic!("missing process must not be signaled")
            ),
            Err(ProcessSignalError::ProcessNotFound)
        );
        assert_eq!(drops.get(), 3);

        write_process_stat(&temp_dir, 123, "test", 500);
        assert_eq!(
            verify_and_send_signal_at(
                &temp_dir,
                identity,
                ProcessSignal::Kill,
                |_| Ok(FakePidFd(Rc::clone(&drops))),
                |_, _| Err(io::Error::from_raw_os_error(libc::EPERM))
            ),
            Err(ProcessSignalError::PermissionDenied)
        );
        assert_eq!(drops.get(), 4);
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn signal_verification_rejects_pid_zero() {
        let temp_dir = std::env::temp_dir();
        let identity = ProcessIdentity {
            pid: 0,
            start_time: 0,
        };
        let mut open_called = false;
        let mut signal_called = false;
        let res = verify_and_send_signal_at(
            &temp_dir,
            identity,
            ProcessSignal::Term,
            |_| {
                open_called = true;
                Ok(())
            },
            |_, _| {
                signal_called = true;
                Ok(())
            },
        );

        assert_eq!(
            res,
            Err(ProcessSignalError::Failed("cannot signal PID 0".into()))
        );
        assert!(!open_called);
        assert!(!signal_called);
    }

    #[test]
    fn parses_process_stat_with_negative_resident_pages() {
        let stat = "123 (test) S 1 123 123 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 500 1000 -5";
        let proc = parse_process_stat(stat, 4096).expect("process stat should parse successfully");
        assert_eq!(proc.pid, 123);
        assert_eq!(proc.memory_bytes, 0);
    }
}
