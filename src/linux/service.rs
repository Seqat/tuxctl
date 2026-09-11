use std::{
    io,
    process::Command,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::Duration,
};

use super::latest_snapshot::{self, LatestReceiver};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectorWake {
    Refresh,
    Stop,
    Timeout,
}

#[derive(Default)]
struct CollectorControlState {
    refresh_pending: bool,
    stop: bool,
}

#[derive(Default)]
struct CollectorControl {
    state: Mutex<CollectorControlState>,
    wake: Condvar,
}

impl CollectorControl {
    fn request_refresh(&self) {
        let mut state = lock(&self.state);
        if state.stop || state.refresh_pending {
            return;
        }
        state.refresh_pending = true;
        drop(state);
        self.wake.notify_one();
    }

    fn stop(&self) {
        let mut state = lock(&self.state);
        state.stop = true;
        drop(state);
        self.wake.notify_one();
    }

    fn wait(&self, timeout: Duration) -> CollectorWake {
        let mut state = lock(&self.state);
        if !state.refresh_pending && !state.stop {
            let (next_state, _) = self
                .wake
                .wait_timeout_while(state, timeout, |state| {
                    !state.refresh_pending && !state.stop
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
        }

        if state.stop {
            CollectorWake::Stop
        } else if std::mem::take(&mut state.refresh_pending) {
            CollectorWake::Refresh
        } else {
            CollectorWake::Timeout
        }
    }
}

pub struct ServiceCollector {
    receiver: LatestReceiver<ServiceSnapshot>,
    control: Arc<CollectorControl>,
    worker: Option<JoinHandle<()>>,
}

impl ServiceCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (snapshot_tx, receiver) = latest_snapshot::channel();
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("systemd-services".into())
            .spawn(move || loop {
                if !snapshot_tx.publish(collect_services()) {
                    break;
                }

                match worker_control.wait(refresh_rate) {
                    CollectorWake::Refresh | CollectorWake::Timeout => {}
                    CollectorWake::Stop => break,
                }
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

    pub fn request_refresh(&self) {
        self.control.request_refresh();
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn collect_services() -> ServiceSnapshot {
    let output = match Command::new("systemctl")
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
        .env("LC_ALL", "C")
        .output()
    {
        Ok(output) => output,
        Err(error) => return ServiceSnapshot::error(format!("cannot run systemctl: {error}")),
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
    }
}

impl ServiceSnapshot {
    fn error(message: String) -> Self {
        Self {
            services: Vec::new(),
            error: Some(message),
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
    fn repeated_refresh_requests_coalesce_to_one_pending_wake() {
        let control = CollectorControl::default();

        for _ in 0..10_000 {
            control.request_refresh();
        }

        assert_eq!(control.wait(Duration::ZERO), CollectorWake::Refresh);
        assert_eq!(control.wait(Duration::ZERO), CollectorWake::Timeout);
    }

    #[test]
    fn stop_takes_priority_over_a_pending_refresh() {
        let control = CollectorControl::default();
        control.request_refresh();

        control.stop();

        assert_eq!(control.wait(Duration::ZERO), CollectorWake::Stop);
    }

    #[test]
    fn stop_wakes_and_joins_a_waiting_worker() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || worker_control.wait(Duration::from_secs(60)));

        control.stop();

        assert_eq!(worker.join().unwrap(), CollectorWake::Stop);
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
