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

fn collected_command(sampler: &mut ProcessSampler, proc_dir: &Path, pid: u32) -> Option<String> {
    sampler
        .collect_at(proc_dir)
        .processes
        .into_iter()
        .find(|process| process.pid == pid)
        .expect("process collected")
        .command
}

fn write_raw_process_stat(proc_dir: &Path, pid: u32, name: &[u8], start_time: u64) {
    let pid_dir = proc_dir.join(pid.to_string());
    fs::create_dir_all(&pid_dir).unwrap();
    let mut stat = format!("{pid} (").into_bytes();
    stat.extend_from_slice(name);
    stat.extend_from_slice(
        format!(
            ") S 1 {pid} {pid} 0 -1 4194304 100 0 0 0 10 20 0 0 20 0 1 0 {start_time} 1000 200"
        )
        .as_bytes(),
    );
    fs::write(pid_dir.join("stat"), stat).unwrap();
}

#[test]
fn processes_with_non_utf8_names_are_listed_and_signalable() {
    let proc_dir = temp_proc_dir("non_utf8_comm");
    let _cleanup = TempDir(proc_dir.clone());
    // Any user can set such a name with prctl(PR_SET_NAME).
    write_raw_process_stat(&proc_dir, 31, b"hid\xffden)\xc3", 700);
    write_process_stat(&proc_dir, 32, "visible", 701);

    let snapshot = ProcessSampler::default().collect_at(&proc_dir);
    let hidden = snapshot
        .processes
        .iter()
        .find(|process| process.pid == 31)
        .expect("a non-UTF-8 name must not hide the process");
    assert_eq!(hidden.name, "hid\u{fffd}den)\u{fffd}");
    assert_eq!(hidden.start_time, 700);
    assert_eq!(snapshot.summary().total, 2);

    let mut sent = None;
    let result = verify_and_send_signal_at(
        &proc_dir,
        hidden.identity(),
        ProcessSignal::Term,
        Ok,
        |pid, signal| {
            sent = Some((*pid, signal));
            Ok(())
        },
    );
    assert_eq!(result, Ok(()));
    assert_eq!(sent, Some((31, libc::SIGTERM)));
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
fn exited_processes_are_evicted_from_cpu_tick_baselines() {
    let proc_dir = temp_proc_dir("ticks_evict");
    let _cleanup = TempDir(proc_dir.clone());
    fs::create_dir_all(&proc_dir).unwrap();
    fs::write(proc_dir.join("stat"), "cpu  10 20 30 40\ncpu0 1 2\n").unwrap();
    write_process_stat(&proc_dir, 10, "short", 500);
    write_process_stat(&proc_dir, 11, "long", 600);
    let mut sampler = ProcessSampler::default();
    sampler.collect_at(&proc_dir);
    assert_eq!(sampler.previous_process_ticks.len(), 2);

    fs::remove_dir_all(proc_dir.join("10")).unwrap();
    sampler.collect_at(&proc_dir);

    assert_eq!(
        sampler.previous_process_ticks.keys().collect::<Vec<_>>(),
        [&ProcessIdentity {
            pid: 11,
            start_time: 600
        }]
    );
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

/// A stat line for pid 42 with the given `flags` field (the 9th field).
fn stat_line_with_flags(flags: &str) -> String {
    format!("42 (worker) S 7 0 0 0 0 {flags} 0 0 0 0 120 30 0 0 0 0 0 0 900 0 25")
}

#[test]
fn kernel_threads_are_detected_from_stat_flags() {
    // kthreadd's typical flags: PF_KTHREAD | PF_NOFREEZE | PF_FORKNOEXEC.
    let kernel = parse_process_stat(&stat_line_with_flags("2129984"), 4096).unwrap();
    assert!(kernel.kernel_thread);

    // A typical user process (PF_RANDOMIZE only), as read from /proc/self/stat.
    let user = parse_process_stat(&stat_line_with_flags("4194304"), 4096).unwrap();
    assert!(!user.kernel_thread);
}

#[test]
fn malformed_stat_flags_keep_the_process_as_a_user_process() {
    let process = parse_process_stat(&stat_line_with_flags("not-a-number"), 4096)
        .expect("unparsable flags must not drop the process");

    assert!(!process.kernel_thread);
    assert_eq!(process.pid, 42);
    assert_eq!(process.start_time, 900);
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
        kernel_thread: false,
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
fn a_name_cannot_forge_the_fields_after_it() {
    // `comm` is set by the process's owner; fields that follow the last `)`
    // are the kernel's, so a name that imitates them changes nothing.
    let forged = "x) R 1 1 1 0 0 0 0 0 0 0 1 1 0 0 0 0 0 0 1 0 1";
    let line = stat_line(forged);
    let process = parse_process_stat(&line, 4096).unwrap();
    assert_eq!(process.name, forged);
    assert_eq!(process.state, 'S');
    assert_eq!(process.parent_pid, 7);
    assert_eq!(process.start_time, 900);
    // The start time is half of the identity that signals are checked against.
    assert_eq!(read_process_start_time(&line), Some(900));

    for name in [")", "((", ") (", "\n)\n", "tab\there", ""] {
        let process = parse_process_stat(&stat_line(name), 4096).unwrap();
        assert_eq!((process.name.as_str(), process.start_time), (name, 900));
    }
}

#[test]
fn truncated_or_malformed_stat_lines_are_skipped() {
    let line = stat_line("worker");
    for cut in [0, 3, 10, line.len() - 10] {
        assert!(parse_process_stat(&line[..cut], 4096).is_none(), "{cut}");
    }
    assert!(parse_process_stat("42 worker S 7", 4096).is_none());
    assert!(parse_process_stat(") 42 (", 4096).is_none());
    assert!(parse_process_stat(&line.replace("900", "-1"), 4096).is_none());
    assert_eq!(read_process_start_time(") 42 ("), None);
}

#[test]
fn huge_command_lines_are_read_only_up_to_a_bound() {
    let proc_dir = temp_proc_dir("huge_cmdline");
    let pid_dir = proc_dir.join("42");
    fs::create_dir_all(&pid_dir).unwrap();
    let mut arguments = b"/usr/bin/demo\0".to_vec();
    arguments.extend(std::iter::repeat_n(b'a', 1 << 20));
    fs::write(pid_dir.join("cmdline"), &arguments).unwrap();

    let command = read_command(&pid_dir).unwrap();
    assert!(
        command.starts_with("/usr/bin/demo aaa"),
        "{}",
        &command[..20]
    );
    assert!(command.ends_with('…'));
    assert!(command.len() <= MAX_COMMAND_BYTES as usize + '…'.len_utf8());

    fs::write(pid_dir.join("cmdline"), b"/usr/bin/demo\0--short\0").unwrap();
    assert_eq!(
        read_command(&pid_dir).as_deref(),
        Some("/usr/bin/demo --short")
    );
    let _ = fs::remove_dir_all(&proc_dir);
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
