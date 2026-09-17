use std::{
    cell::Cell,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, OnceLock,
    },
    thread::{self, JoinHandle},
};

use serde_json::Value;

const CHANNEL_CAPACITY: usize = 512;
const JOURNAL_MAX_BATCH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub id: u64,
    pub timestamp_micros: Option<u64>,
    pub source: String,
    pub priority: Option<u8>,
    pub message: String,
}

impl JournalEntry {
    pub fn priority_label(&self) -> &'static str {
        priority_label(self.priority)
    }
}

pub fn priority_label(priority: Option<u8>) -> &'static str {
    match priority {
        Some(0) => "emerg",
        Some(1) => "alert",
        Some(2) => "crit",
        Some(3) => "error",
        Some(4) => "warn",
        Some(5) => "notice",
        Some(6) => "info",
        Some(7) => "debug",
        _ => "-",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalBatch {
    pub entries: Vec<JournalEntry>,
    pub dropped: usize,
    pub error: Option<String>,
}

pub struct JournalCollector {
    receiver: Receiver<JournalEntry>,
    dropped: Arc<AtomicUsize>,
    terminal_error: Arc<OnceLock<String>>,
    terminal_error_delivered: Cell<bool>,
    child: Option<Child>,
    worker: Option<JoinHandle<()>>,
}

impl JournalCollector {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicUsize::new(0));
        let terminal_error = Arc::new(OnceLock::new());
        let mut child = match spawn_journalctl() {
            Ok(child) => child,
            Err(error) => {
                report_terminal_error(&terminal_error, format!("cannot start journalctl: {error}"));
                return Self {
                    receiver,
                    dropped,
                    terminal_error,
                    terminal_error_delivered: Cell::new(false),
                    child: None,
                    worker: None,
                };
            }
        };

        let Some(stdout) = child.stdout.take() else {
            report_terminal_error(
                &terminal_error,
                "journalctl did not provide an output stream",
            );
            let _ = child.kill();
            return Self {
                receiver,
                dropped,
                terminal_error,
                terminal_error_delivered: Cell::new(false),
                child: Some(child),
                worker: None,
            };
        };
        let stderr = child.stderr.take();

        let worker_dropped = Arc::clone(&dropped);
        let worker_terminal_error = Arc::clone(&terminal_error);
        let worker = match thread::Builder::new()
            .name("journal-stream".into())
            .spawn(move || {
                read_journal(
                    stdout,
                    stderr,
                    sender,
                    &worker_dropped,
                    &worker_terminal_error,
                )
            }) {
            Ok(worker) => Some(worker),
            Err(error) => {
                report_terminal_error(
                    &terminal_error,
                    format!("cannot start journal reader thread: {error}"),
                );
                let _ = child.kill();
                None
            }
        };

        Self {
            receiver,
            dropped,
            terminal_error,
            terminal_error_delivered: Cell::new(false),
            child: Some(child),
            worker,
        }
    }

    pub fn latest(&self) -> Option<JournalBatch> {
        let drain = drain_journal_entries(|| self.receiver.try_recv());
        if drain.disconnected || self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            report_terminal_error(&self.terminal_error, "journal reader stopped unexpectedly");
        }

        let error = if self.terminal_error_delivered.get() {
            None
        } else {
            self.terminal_error.get().cloned().inspect(|_| {
                self.terminal_error_delivered.set(true);
            })
        };
        let batch = JournalBatch {
            entries: drain.entries,
            dropped: self.dropped.swap(0, Ordering::Relaxed),
            error,
        };

        (!batch.entries.is_empty() || batch.dropped > 0 || batch.error.is_some()).then_some(batch)
    }
}

struct JournalDrain {
    entries: Vec<JournalEntry>,
    disconnected: bool,
}

fn drain_journal_entries(
    mut try_receive: impl FnMut() -> Result<JournalEntry, TryRecvError>,
) -> JournalDrain {
    let mut entries = Vec::with_capacity(JOURNAL_MAX_BATCH);
    let mut disconnected = false;

    for _ in 0..JOURNAL_MAX_BATCH {
        match try_receive() {
            Ok(entry) => entries.push(entry),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    JournalDrain {
        entries,
        disconnected,
    }
}

fn report_terminal_error(terminal_error: &OnceLock<String>, error: impl Into<String>) {
    let _ = terminal_error.set(error.into());
}

impl Drop for JournalCollector {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn spawn_journalctl() -> std::io::Result<Child> {
    Command::new("journalctl")
        .args([
            "--no-pager",
            "--quiet",
            "--follow",
            "--output=json",
            "--output-fields=__REALTIME_TIMESTAMP,_SYSTEMD_UNIT,_SYSTEMD_USER_UNIT,SYSLOG_IDENTIFIER,_COMM,PRIORITY,MESSAGE",
            "--lines=200",
        ])
        .env("SYSTEMD_COLORS", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn read_journal<R, E>(
    stdout: R,
    mut stderr: Option<E>,
    sender: SyncSender<JournalEntry>,
    dropped: &AtomicUsize,
    terminal_error: &OnceLock<String>,
) where
    R: Read,
    E: Read,
{
    let mut next_id = 1_u64;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                report_terminal_error(
                    terminal_error,
                    format!("journal stream read failed: {error}"),
                );
                return;
            }
        };

        let Some(entry) = parse_journal_json(&line, next_id) else {
            dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        next_id = next_id.wrapping_add(1).max(1);
        match sender.try_send(entry) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }

    report_terminal_error(terminal_error, "journal stream ended");
    if let Some(stderr) = &mut stderr {
        let _ = std::io::copy(stderr, &mut std::io::sink());
    }
}

fn parse_journal_json(line: &str, id: u64) -> Option<JournalEntry> {
    let object = serde_json::from_str::<Value>(line).ok()?;
    let timestamp_micros =
        field(&object, "__REALTIME_TIMESTAMP").and_then(|value| value.parse().ok());
    let source = [
        "_SYSTEMD_UNIT",
        "_SYSTEMD_USER_UNIT",
        "SYSLOG_IDENTIFIER",
        "_COMM",
    ]
    .into_iter()
    .find_map(|name| field(&object, name))
    .unwrap_or_else(|| "journal".into());
    let priority = field(&object, "PRIORITY").and_then(|value| value.parse::<u8>().ok());
    let message = field(&object, "MESSAGE").unwrap_or_else(|| "<binary or empty message>".into());

    Some(JournalEntry {
        id,
        timestamp_micros,
        source,
        priority,
        message,
    })
}

fn field(object: &Value, name: &str) -> Option<String> {
    object.get(name)?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64) -> JournalEntry {
        JournalEntry {
            id,
            timestamp_micros: Some(id),
            source: "test".into(),
            priority: Some(6),
            message: format!("entry {id}"),
        }
    }

    fn entry_ids(entries: &[JournalEntry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.id).collect()
    }

    fn collector_for_test(
        receiver: Receiver<JournalEntry>,
        dropped: Arc<AtomicUsize>,
        terminal_error: Arc<OnceLock<String>>,
    ) -> JournalCollector {
        JournalCollector {
            receiver,
            dropped,
            terminal_error,
            terminal_error_delivered: Cell::new(false),
            child: None,
            worker: None,
        }
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("simulated read failure"))
        }
    }

    struct StatusCheckingEofReader {
        terminal_error: Arc<OnceLock<String>>,
    }

    impl Read for StatusCheckingEofReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            assert_eq!(
                self.terminal_error.get().map(String::as_str),
                Some("journal stream ended")
            );
            Ok(0)
        }
    }

    #[test]
    fn parses_structured_journal_entry_with_unit() {
        let entry = parse_journal_json(
            r#"{"__REALTIME_TIMESTAMP":"1720000123456789","_SYSTEMD_UNIT":"sshd.service","SYSLOG_IDENTIFIER":"sshd","PRIORITY":"4","MESSAGE":"Login failed: a=b"}"#,
            9,
        )
        .unwrap();

        assert_eq!(entry.id, 9);
        assert_eq!(entry.timestamp_micros, Some(1_720_000_123_456_789));
        assert_eq!(entry.source, "sshd.service");
        assert_eq!(entry.priority, Some(4));
        assert_eq!(entry.message, "Login failed: a=b");
    }

    #[test]
    fn falls_back_to_process_source_and_handles_optional_fields() {
        let entry =
            parse_journal_json(r#"{"_COMM":"kernel-worker","MESSAGE":[1,2,3]}"#, 1).unwrap();

        assert_eq!(entry.timestamp_micros, None);
        assert_eq!(entry.source, "kernel-worker");
        assert_eq!(entry.priority, None);
        assert_eq!(entry.message, "<binary or empty message>");
    }

    #[test]
    fn rejects_malformed_json_without_panicking() {
        assert_eq!(parse_journal_json("not-json", 1), None);
    }

    #[test]
    fn empty_channel_produces_an_empty_drain() {
        let (_sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);

        let drain = drain_journal_entries(|| receiver.try_recv());

        assert!(drain.entries.is_empty());
        assert!(!drain.disconnected);
    }

    #[test]
    fn drain_returns_every_available_entry_below_the_limit_in_fifo_order() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        for id in 1..JOURNAL_MAX_BATCH as u64 {
            sender.try_send(entry(id)).unwrap();
        }

        let drain = drain_journal_entries(|| receiver.try_recv());

        assert_eq!(
            entry_ids(&drain.entries),
            (1..JOURNAL_MAX_BATCH as u64).collect::<Vec<_>>()
        );
        assert!(!drain.disconnected);
    }

    #[test]
    fn bounded_drains_leave_remaining_entries_and_preserve_global_fifo_order() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let total = JOURNAL_MAX_BATCH * 2 + 5;
        for id in 1..=total as u64 {
            sender.try_send(entry(id)).unwrap();
        }

        let first = drain_journal_entries(|| receiver.try_recv());
        let second = drain_journal_entries(|| receiver.try_recv());
        let third = drain_journal_entries(|| receiver.try_recv());
        let mut all_ids = entry_ids(&first.entries);
        all_ids.extend(entry_ids(&second.entries));
        all_ids.extend(entry_ids(&third.entries));

        assert_eq!(first.entries.len(), JOURNAL_MAX_BATCH);
        assert_eq!(second.entries.len(), JOURNAL_MAX_BATCH);
        assert_eq!(third.entries.len(), 5);
        assert_eq!(all_ids, (1..=total as u64).collect::<Vec<_>>());
    }

    #[test]
    fn continuously_ready_source_cannot_exceed_one_batch_budget() {
        let mut next_id = 1_u64;

        let drain = drain_journal_entries(|| {
            let next = entry(next_id);
            next_id += 1;
            Ok(next)
        });

        assert_eq!(drain.entries.len(), JOURNAL_MAX_BATCH);
        assert_eq!(
            entry_ids(&drain.entries),
            (1..=JOURNAL_MAX_BATCH as u64).collect::<Vec<_>>()
        );
        assert_eq!(next_id, JOURNAL_MAX_BATCH as u64 + 1);
    }

    #[test]
    fn full_entry_queue_cannot_hide_reader_failure() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        for id in 1..=CHANNEL_CAPACITY as u64 {
            sender.try_send(entry(id)).unwrap();
        }
        assert!(matches!(
            sender.try_send(entry(0)),
            Err(TrySendError::Full(_))
        ));

        let dropped = Arc::new(AtomicUsize::new(0));
        let terminal_error = Arc::new(OnceLock::new());
        read_journal(
            FailingReader,
            None::<std::io::Empty>,
            sender,
            &dropped,
            &terminal_error,
        );
        let collector = collector_for_test(receiver, dropped, Arc::clone(&terminal_error));

        let batch = collector
            .latest()
            .expect("entries and error are observable");
        assert_eq!(batch.entries.len(), JOURNAL_MAX_BATCH);
        assert_eq!(
            batch.error.as_deref(),
            Some("journal stream read failed: simulated read failure")
        );
        assert_eq!(
            terminal_error.get().map(String::as_str),
            Some("journal stream read failed: simulated read failure")
        );
    }

    #[test]
    fn no_data_and_terminal_failure_are_distinct_states() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicUsize::new(0));
        let terminal_error = Arc::new(OnceLock::new());
        let collector = collector_for_test(receiver, dropped, Arc::clone(&terminal_error));

        assert_eq!(collector.latest(), None);

        report_terminal_error(&terminal_error, "journal stream ended");
        drop(sender);
        let batch = collector.latest().expect("terminal state is observable");
        assert!(batch.entries.is_empty());
        assert_eq!(batch.error.as_deref(), Some("journal stream ended"));
        assert_eq!(collector.latest(), None);
    }

    #[test]
    fn unexplained_sender_disconnect_reports_terminal_failure_once() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicUsize::new(0));
        let terminal_error = Arc::new(OnceLock::new());
        let collector = collector_for_test(receiver, dropped, terminal_error);
        drop(sender);

        let batch = collector.latest().expect("disconnect is observable");

        assert_eq!(
            batch.error.as_deref(),
            Some("journal reader stopped unexpectedly")
        );
        assert_eq!(collector.latest(), None);
    }

    #[test]
    fn stream_eof_reports_terminal_status_before_reading_stderr() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicUsize::new(0));
        let terminal_error = Arc::new(OnceLock::new());
        read_journal(
            std::io::Cursor::new(Vec::<u8>::new()),
            Some(StatusCheckingEofReader {
                terminal_error: Arc::clone(&terminal_error),
            }),
            sender,
            &dropped,
            &terminal_error,
        );
        let collector = collector_for_test(receiver, dropped, terminal_error);

        let batch = collector.latest().expect("EOF status is observable");

        assert_eq!(batch.error.as_deref(), Some("journal stream ended"));
    }

    #[test]
    fn reader_stops_cleanly_when_consumer_disconnects() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let dropped = AtomicUsize::new(0);
        let terminal_error = OnceLock::new();
        drop(receiver);
        let input = br#"{"MESSAGE":"entry after shutdown"}
"#;

        read_journal(
            std::io::Cursor::new(input),
            None::<std::io::Empty>,
            sender,
            &dropped,
            &terminal_error,
        );

        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert!(terminal_error.get().is_none());
    }

    #[test]
    fn collector_shutdown_with_pending_terminal_failure_completes() {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        for id in 1..=CHANNEL_CAPACITY as u64 {
            sender.try_send(entry(id)).unwrap();
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let terminal_error = Arc::new(OnceLock::new());
        report_terminal_error(&terminal_error, "pending failure");
        let collector = collector_for_test(receiver, dropped, terminal_error);

        drop(collector);
        drop(sender);
    }
}
