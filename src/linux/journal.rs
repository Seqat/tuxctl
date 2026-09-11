use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
};

use serde_json::Value;

const CHANNEL_CAPACITY: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub id: u64,
    pub timestamp_micros: Option<u64>,
    pub source: String,
    pub priority: Option<u8>,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalBatch {
    pub entries: Vec<JournalEntry>,
    pub dropped: usize,
    pub error: Option<String>,
}

enum JournalEvent {
    Entry(JournalEntry),
    Error(String),
}

pub struct JournalCollector {
    receiver: Receiver<JournalEvent>,
    dropped: Arc<AtomicUsize>,
    child: Option<Child>,
    worker: Option<JoinHandle<()>>,
}

impl JournalCollector {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut child = match spawn_journalctl() {
            Ok(child) => child,
            Err(error) => {
                let _ = sender.try_send(JournalEvent::Error(format!(
                    "cannot start journalctl: {error}"
                )));
                return Self {
                    receiver,
                    dropped,
                    child: None,
                    worker: None,
                };
            }
        };

        let Some(stdout) = child.stdout.take() else {
            let _ = sender.try_send(JournalEvent::Error(
                "journalctl did not provide an output stream".into(),
            ));
            let _ = child.kill();
            return Self {
                receiver,
                dropped,
                child: Some(child),
                worker: None,
            };
        };
        let stderr = child.stderr.take();

        let worker_dropped = Arc::clone(&dropped);
        let error_sender = sender.clone();
        let worker = thread::Builder::new()
            .name("journal-stream".into())
            .spawn(move || read_journal(stdout, stderr, sender, &worker_dropped))
            .ok();
        if worker.is_none() {
            let _ = error_sender.try_send(JournalEvent::Error(
                "cannot start journal reader thread".into(),
            ));
            let _ = child.kill();
        }

        Self {
            receiver,
            dropped,
            child: Some(child),
            worker,
        }
    }

    pub fn latest(&self) -> Option<JournalBatch> {
        let mut batch = JournalBatch {
            dropped: self.dropped.swap(0, Ordering::Relaxed),
            ..JournalBatch::default()
        };

        for event in self.receiver.try_iter() {
            match event {
                JournalEvent::Entry(entry) => batch.entries.push(entry),
                JournalEvent::Error(error) => batch.error = Some(error),
            }
        }

        (!batch.entries.is_empty() || batch.dropped > 0 || batch.error.is_some()).then_some(batch)
    }
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

fn read_journal(
    stdout: ChildStdout,
    mut stderr: Option<ChildStderr>,
    sender: SyncSender<JournalEvent>,
    dropped: &AtomicUsize,
) {
    let mut next_id = 1_u64;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                let _ = sender.try_send(JournalEvent::Error(format!(
                    "journal stream read failed: {error}"
                )));
                return;
            }
        };

        let Some(entry) = parse_journal_json(&line, next_id) else {
            dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        next_id = next_id.wrapping_add(1).max(1);
        match sender.try_send(JournalEvent::Entry(entry)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }

    let mut error = String::new();
    if let Some(stderr) = &mut stderr {
        let _ = stderr.read_to_string(&mut error);
    }
    let error = error
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("journal stream ended");
    let _ = sender.try_send(JournalEvent::Error(error.into()));
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
}
