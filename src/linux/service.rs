use std::{
    io,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

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

enum CollectorCommand {
    Refresh,
    Stop,
}

pub struct ServiceCollector {
    receiver: Receiver<ServiceSnapshot>,
    commands: Sender<CollectorCommand>,
    worker: Option<JoinHandle<()>>,
}

impl ServiceCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (snapshot_tx, receiver) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("systemd-services".into())
            .spawn(move || loop {
                if snapshot_tx.send(collect_services()).is_err() {
                    break;
                }

                match command_rx.recv_timeout(refresh_rate) {
                    Ok(CollectorCommand::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(CollectorCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })?;

        Ok(Self {
            receiver,
            commands,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<ServiceSnapshot> {
        self.receiver.try_iter().last()
    }

    pub fn request_refresh(&self) {
        let _ = self.commands.send(CollectorCommand::Refresh);
    }
}

impl Drop for ServiceCollector {
    fn drop(&mut self) {
        let _ = self.commands.send(CollectorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
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
