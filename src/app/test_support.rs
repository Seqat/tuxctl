//! Fixtures shared by the app module tests.

use super::*;

pub(super) fn process(pid: u32, name: &str) -> ProcessInfo {
    ProcessInfo {
        pid,
        name: name.into(),
        cpu_percent: Some(1.0),
        memory_bytes: 1024,
        command: Some(format!("/usr/bin/{name}")),
        state: "S (sleeping)".into(),
        parent_pid: 1,
        state_code: 'S',
        start_time: u64::from(pid),
    }
}

pub(super) fn processes(entries: Vec<ProcessInfo>) -> ProcessSnapshot {
    ProcessSnapshot {
        processes: entries,
        error: None,
    }
}

pub(super) fn service(unit: &str, active_state: &str, description: &str) -> ServiceInfo {
    ServiceInfo {
        unit: unit.into(),
        load_state: "loaded".into(),
        active_state: active_state.into(),
        sub_state: if active_state == "active" {
            "running".into()
        } else {
            "dead".into()
        },
        description: description.into(),
    }
}

pub(super) fn services(entries: Vec<ServiceInfo>) -> ServiceSnapshot {
    ServiceSnapshot {
        services: entries,
        error: None,
        completed_refresh_generation: 0,
    }
}

pub(super) fn services_completed(
    entries: Vec<ServiceInfo>,
    generation: ServiceRefreshGeneration,
) -> ServiceSnapshot {
    ServiceSnapshot {
        completed_refresh_generation: generation,
        ..services(entries)
    }
}

pub(super) fn visible_units(app: &App) -> Vec<&str> {
    (0..app.service_count())
        .filter_map(|index| app.service_at(index).map(|service| service.unit.as_str()))
        .collect()
}

pub(super) fn log_entry(id: u64, source: &str, priority: u8, message: &str) -> JournalEntry {
    JournalEntry {
        id,
        timestamp_micros: Some(id.saturating_mul(1_000_000)),
        source: source.into(),
        priority: Some(priority),
        message: message.into(),
    }
}

pub(super) fn log_batch(entries: Vec<JournalEntry>) -> JournalBatch {
    JournalBatch {
        entries,
        dropped: 0,
        error: None,
    }
}

pub(super) fn visible_log_ids(app: &App) -> Vec<u64> {
    (0..app.log_count())
        .filter_map(|index| app.log_at(index).map(|entry| entry.id))
        .collect()
}

pub(super) fn process_with(pid: u32, name: &str, cpu: Option<f64>, memory: u64) -> ProcessInfo {
    let mut process = process(pid, name);
    process.cpu_percent = cpu;
    process.memory_bytes = memory;
    process
}

pub(super) fn visible_pids(app: &App) -> Vec<u32> {
    (0..app.process_count())
        .filter_map(|index| app.process_at(index).map(|process| process.pid))
        .collect()
}

pub(super) fn dummy_network(name: &str) -> NetworkInterfaceInfo {
    NetworkInterfaceInfo {
        name: name.to_string(),
        operstate: crate::linux::OperState::Up,
        mac_address: Some("00:11:22:33:44:55".to_string()),
        mtu: Some(1500),
        ipv4_addresses: Vec::new(),
        ipv6_addresses: Vec::new(),
        rx_bytes: 1000,
        tx_bytes: 1000,
        rx_packets: 10,
        tx_packets: 10,
        rx_errors: 0,
        tx_errors: 0,
        rx_dropped: 0,
        tx_dropped: 0,
        rx_rate_bytes_per_sec: Some(100.0),
        tx_rate_bytes_per_sec: Some(100.0),
    }
}
