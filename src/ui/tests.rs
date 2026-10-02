use ratatui::{backend::TestBackend, Terminal};

use std::path::Path;

use super::*;
use crate::action::Action;
use crate::linux::{ByteUsage, LogicalCpuId, LogicalCpuMetrics, ProcessIdentity, SystemMetrics};

fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn process_confirmation_app() -> App {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
        processes: vec![crate::linux::ProcessInfo {
            pid: 1234,
            name: "testproc".into(),
            cpu_percent: Some(5.0),
            memory_bytes: 4096,
            command: Some("/bin/testproc".into()),
            state: "R (running)".into(),
            parent_pid: 1,
            state_code: 'R',
            start_time: 100,
            kernel_thread: false,
        }],
        error: None,
    }));
    app.update(Action::RequestProcessSignal(
        crate::linux::ProcessSignal::Term,
    ));
    app
}

fn rendered_regions(terminal: &mut Terminal<TestBackend>, app: &App) -> UiRegions {
    let mut regions = UiRegions::default();
    terminal
        .draw(|frame| {
            regions = render(frame, app);
        })
        .unwrap();
    regions
}

#[test]
fn hit_testing_includes_top_left_and_excludes_bottom_right() {
    let regions = UiRegions::from_tabs([(Tab::Overview, Rect::new(10, 5, 8, 2))]);

    assert_eq!(
        regions.target_at(10, 5),
        Some(MouseTarget::Tab(Tab::Overview))
    );
    assert_eq!(
        regions.target_at(17, 6),
        Some(MouseTarget::Tab(Tab::Overview))
    );
    assert_eq!(regions.target_at(18, 6), None);
    assert_eq!(regions.target_at(17, 7), None);
}

#[test]
fn hit_testing_does_not_overflow_at_terminal_limits() {
    let last_cell = u16::MAX - 1;
    let regions = UiRegions::from_tabs([(Tab::Network, Rect::new(last_cell, last_cell, 1, 1))]);

    assert_eq!(
        regions.target_at(last_cell, last_cell),
        Some(MouseTarget::Tab(Tab::Network))
    );
}

#[test]
fn process_header_hit_testing_excludes_borders_and_spacing() {
    let regions = UiRegions::from_process_headers([
        (ProcessSortField::Pid, Rect::new(2, 4, 8, 1)),
        (ProcessSortField::Name, Rect::new(12, 4, 16, 1)),
    ]);

    assert_eq!(regions.target_at(1, 4), None);
    assert_eq!(
        regions.target_at(2, 4),
        Some(MouseTarget::ProcessSortHeader(ProcessSortField::Pid))
    );
    assert_eq!(regions.target_at(10, 4), None);
    assert_eq!(regions.target_at(11, 4), None);
    assert_eq!(regions.target_at(12, 3), None);
    assert_eq!(
        regions.target_at(12, 4),
        Some(MouseTarget::ProcessSortHeader(ProcessSortField::Name))
    );
    assert_eq!(regions.target_at(12, 5), None);
}

#[test]
fn process_row_hit_testing_excludes_adjacent_lines() {
    let identity = ProcessIdentity {
        pid: 123,
        start_time: 456,
    };
    let regions =
        UiRegions::from_process_rows([(identity, Rect::new(2, 6, 40, 1))], InputMode::Normal);

    assert_eq!(regions.target_at(1, 6), None);
    assert_eq!(regions.target_at(2, 5), None);
    assert_eq!(
        regions.target_at(2, 6),
        Some(MouseTarget::ProcessRow(identity))
    );
    assert_eq!(regions.target_at(42, 6), None);
    assert_eq!(regions.target_at(2, 7), None);
}

#[test]
fn service_row_hit_testing_uses_unit_identity_and_excludes_borders() {
    let regions = UiRegions::from_service_rows(
        [(Arc::from("dbus.service"), Rect::new(2, 6, 40, 1))],
        InputMode::Services,
    );

    assert_eq!(regions.target_at(1, 6), None);
    assert_eq!(
        regions.target_at(2, 6),
        Some(MouseTarget::ServiceRow(Arc::from("dbus.service")))
    );
    assert_eq!(regions.target_at(42, 6), None);
    assert_eq!(regions.target_at(2, 7), None);
}

#[test]
fn log_row_hit_testing_uses_entry_identity_and_excludes_borders() {
    let regions = UiRegions::from_log_rows([(77, Rect::new(2, 6, 40, 1))], InputMode::Logs);

    assert_eq!(regions.target_at(1, 6), None);
    assert_eq!(regions.target_at(2, 6), Some(MouseTarget::LogRow(77)));
    assert_eq!(regions.target_at(42, 6), None);
    assert_eq!(regions.target_at(2, 7), None);
}

#[test]
fn formats_uptime_at_day_hour_and_minute_boundaries() {
    let uptime = std::time::Duration::from_secs(3 * 86_400 + 14 * 3_600 + 22 * 60);

    assert_eq!(format_uptime(uptime), "3d 14h 22m");
    assert_eq!(
        format_uptime(std::time::Duration::from_secs(65 * 60)),
        "1h 5m"
    );
    assert_eq!(format_uptime(std::time::Duration::from_secs(59)), "0m");
}

#[test]
fn formats_bytes_with_binary_units() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(1024), "1.0 KiB");
    assert_eq!(format_bytes(10 * 1024 * 1024 * 1024), "10.0 GiB");
}

#[test]
fn network_row_hit_testing_uses_interface_name_and_excludes_borders() {
    let regions = UiRegions::from_network_rows(
        [(Arc::from("enp6s0"), Rect::new(2, 6, 40, 1))],
        InputMode::Network,
    );

    assert_eq!(regions.target_at(1, 6), None);
    assert_eq!(
        regions.target_at(2, 6),
        Some(MouseTarget::NetworkRow(Arc::from("enp6s0")))
    );
    assert_eq!(regions.target_at(42, 6), None);
    assert_eq!(regions.target_at(2, 7), None);
}

#[test]
fn modal_suppression_clears_background_targets_but_preserves_viewports() {
    let identity = ProcessIdentity {
        pid: 42,
        start_time: 9001,
    };
    let target_area = Rect::new(2, 6, 20, 1);
    let mut regions = UiRegions {
        tabs: vec![TabRegion {
            tab: Tab::Overview,
            area: target_area,
        }],
        process_rows: vec![ProcessRowRegion {
            identity,
            area: target_area,
            pin_up: Some(Rect::new(10, 6, 2, 1)),
            pin_down: Some(Rect::new(12, 6, 2, 1)),
        }],
        process_headers: vec![ProcessHeaderRegion {
            field: ProcessSortField::Cpu,
            area: target_area,
        }],
        process_scroll_area: Some(target_area),
        process_viewport: Some((3, 7)),
        service_rows: vec![ServiceRowRegion {
            unit: Arc::from("dbus.service"),
            area: target_area,
        }],
        service_scroll_area: Some(target_area),
        service_viewport: Some((4, 8)),
        log_rows: vec![LogRowRegion {
            id: 77,
            area: target_area,
        }],
        log_scroll_area: Some(target_area),
        log_viewport: Some((5, 9)),
        network_rows: vec![NetworkRowRegion {
            name: Arc::from("enp6s0"),
            area: target_area,
        }],
        network_scroll_area: Some(target_area),
        network_viewport: Some((6, 10)),
        process_signal_cancel: None,
        process_signal_confirm: None,
        menu_items: Vec::new(),
        interval_buttons: vec![(IntervalStep::Longer, target_area)],
        input_mode: InputMode::ProcessSignalConfirm,
    };

    regions.suppress_background_interaction();

    assert!(regions.tabs.is_empty());
    assert!(regions.process_rows.is_empty());
    assert!(regions.process_headers.is_empty());
    assert!(regions.process_scroll_area.is_none());
    assert!(regions.service_rows.is_empty());
    assert!(regions.service_scroll_area.is_none());
    assert!(regions.log_rows.is_empty());
    assert!(regions.log_scroll_area.is_none());
    assert!(regions.network_rows.is_empty());
    assert!(regions.network_scroll_area.is_none());
    assert_eq!(regions.process_viewport(), Some((3, 7)));
    assert_eq!(regions.service_viewport(), Some((4, 8)));
    assert_eq!(regions.log_viewport(), Some((5, 9)));
    assert_eq!(regions.network_viewport(), Some((6, 10)));
    assert_eq!(regions.target_at(2, 6), None);
    assert_eq!(regions.target_at(10, 6), None, "pin controls are gone too");

    let cancel = Rect::new(10, 12, 12, 1);
    let confirm = Rect::new(26, 12, 15, 1);
    regions.process_signal_cancel = Some(cancel);
    regions.process_signal_confirm = Some(confirm);
    assert_eq!(
        regions.target_at(cancel.x, cancel.y),
        Some(MouseTarget::ProcessSignalCancel)
    );
    assert_eq!(
        regions.target_at(confirm.x, confirm.y),
        Some(MouseTarget::ProcessSignalConfirm)
    );
}

fn populated_app(tab: Tab) -> App {
    let mut app = App::default();
    app.update(Action::SystemMetricsUpdated(SystemMetrics {
        memory: Some(ByteUsage {
            used: 4 << 30,
            total: 16 << 30,
        }),
        mounts: vec![crate::linux::MountUsage {
            mount_point: "/".into(),
            usage: crate::linux::ByteUsage {
                used: 50 << 30,
                total: 100 << 30,
            },
        }],
        ..SystemMetrics::default()
    }));
    app.update(Action::NetworkUpdated(crate::linux::NetworkSnapshot {
        interfaces: vec![crate::linux::NetworkInterfaceInfo {
            name: "lo".into(),
            operstate: crate::linux::OperState::Unknown,
            mac_address: None,
            mtu: Some(65536),
            ipv4_addresses: Vec::new(),
            ipv6_addresses: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            rx_packets: 0,
            tx_packets: 0,
            rx_errors: 0,
            tx_errors: 0,
            rx_dropped: 0,
            tx_dropped: 0,
            rx_rate_bytes_per_sec: None,
            tx_rate_bytes_per_sec: None,
        }],
        error: None,
    }));
    app.update(Action::LogsUpdated(crate::linux::JournalBatch {
        entries: vec![crate::linux::JournalEntry {
            id: 1,
            timestamp_micros: Some(1_000_000),
            local_time: None,
            source: "sshd.service".into(),
            priority: Some(4),
            message: "warning message".into(),
        }],
        dropped: 0,
        error: None,
    }));
    app.update(Action::SelectTab(tab));
    app
}

/// Text controlled by other local users, with escape sequences a terminal
/// would act on: clipboard write (OSC 52), full reset, 8-bit CSI, bidi.
const HOSTILE: &str = "ev\u{1b}]52;c;cm0gLXJmIH4K\u{7}il\u{1b}c\u{9b}2J\t\r\u{202E}x";

fn hostile_app() -> App {
    let mut app = App::default();
    app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
        processes: vec![crate::linux::ProcessInfo {
            pid: 1234,
            name: HOSTILE.into(),
            cpu_percent: Some(5.0),
            memory_bytes: 4096,
            command: Some(HOSTILE.into()),
            state: "R (running)".into(),
            parent_pid: 1,
            state_code: 'R',
            start_time: 100,
            kernel_thread: false,
        }],
        error: None,
    }));
    app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
        services: vec![crate::linux::ServiceInfo {
            unit: format!("{HOSTILE}.service"),
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            description: HOSTILE.into(),
        }],
        error: None,
        completed_refresh_generation: 0,
    }));
    app.update(Action::LogsUpdated(crate::linux::JournalBatch {
        entries: vec![crate::linux::JournalEntry {
            id: 1,
            timestamp_micros: Some(1_000_000),
            local_time: None,
            source: HOSTILE.into(),
            priority: Some(3),
            message: format!("{HOSTILE}\nsecond {HOSTILE}"),
        }],
        dropped: 0,
        error: None,
    }));
    app.update(Action::NetworkUpdated(crate::linux::NetworkSnapshot {
        interfaces: vec![crate::linux::NetworkInterfaceInfo {
            name: HOSTILE.into(),
            operstate: crate::linux::OperState::Up,
            mac_address: Some(HOSTILE.into()),
            mtu: Some(1500),
            ipv4_addresses: Vec::new(),
            ipv6_addresses: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            rx_packets: 0,
            tx_packets: 0,
            rx_errors: 0,
            tx_errors: 0,
            rx_dropped: 0,
            tx_dropped: 0,
            rx_rate_bytes_per_sec: None,
            tx_rate_bytes_per_sec: None,
        }],
        error: None,
    }));
    app
}

#[test]
fn hostile_strings_never_reach_the_terminal_buffer() {
    let overlays: [(Tab, Option<Action>); 10] = [
        (Tab::Overview, None),
        (Tab::Processes, None),
        (Tab::Processes, Some(Action::OpenProcessDetails)),
        (
            Tab::Processes,
            Some(Action::RequestProcessSignal(
                crate::linux::ProcessSignal::Kill,
            )),
        ),
        (Tab::Services, None),
        (Tab::Services, Some(Action::OpenServiceDetails)),
        (Tab::Logs, None),
        (Tab::Logs, Some(Action::OpenLogDetails)),
        (Tab::Network, None),
        (Tab::Network, Some(Action::OpenNetworkDetails)),
    ];
    for (width, height) in [(120, 40), (40, 15)] {
        for (tab, open) in overlays.clone() {
            let mut app = hostile_app();
            app.update(Action::SelectTab(tab));
            if let Some(open) = open.clone() {
                assert!(app.update(open.clone()), "{open:?} did not open");
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            rendered_regions(&mut terminal, &app);

            let buffer = terminal.backend().buffer();
            assert!(
                !sanitize::has_unsafe_symbol(buffer),
                "{tab:?} {open:?} at {width}x{height} leaks a control character"
            );
            if width == 120 && tab != Tab::Overview {
                assert!(
                    buffer_text(&terminal).contains("ev"),
                    "{tab:?} {open:?}: the hostile text was not rendered at all"
                );
            }
        }
    }
}

#[test]
fn stale_marker_is_shown_in_the_frame_without_hiding_search_state() {
    let mut app = App::default().with_collector_periods(
        crate::app::CollectorPeriods::for_sampling_interval(std::time::Duration::from_secs(1)),
    );
    app.update(Action::SelectTab(Tab::Processes));
    app.update(Action::BeginProcessSearch);
    app.update(Action::AppendProcessSearch('x'));
    let start = std::time::Instant::now();
    app.update(Action::Tick(start));
    app.update(Action::Tick(start + std::time::Duration::from_secs(10)));

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    rendered_regions(&mut terminal, &app);
    let text = buffer_text(&terminal);
    assert!(text.contains(" stale: metrics, processes "));
    assert!(text.contains("Search: x_"));

    app.update(Action::SelectTab(Tab::Overview));
    let mut narrow = Terminal::new(TestBackend::new(40, 15)).unwrap();
    rendered_regions(&mut narrow, &app);
    let text = buffer_text(&narrow);
    assert!(
        text.contains(" stale "),
        "three names fall back to a bare marker"
    );
    assert!(text.contains(" tuxctl "));
}

#[test]
fn an_open_popup_dims_the_screen_behind_it() {
    // The border and tab rows lie outside every popup.
    let dimmed = |terminal: &Terminal<TestBackend>| {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width).all(|x| {
            (0..2).all(|y| {
                let cell = &buffer[(x, y)];
                cell.fg == theme::BACKDROP_FG && cell.bg == theme::BACKDROP_BG
            })
        })
    };
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    rendered_regions(&mut terminal, &app);
    assert!(!dimmed(&terminal));

    app.update(Action::Escape);
    rendered_regions(&mut terminal, &app);
    assert!(dimmed(&terminal));
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.bg == theme::SELECTED_BG),
        "the menu keeps its colors"
    );

    app.update(Action::Escape);
    rendered_regions(&mut terminal, &app);
    assert!(!dimmed(&terminal), "closing the menu restores colors");
}

#[test]
fn dimming_is_safe_in_tiny_terminals() {
    let mut app = App::default();
    app.update(Action::Escape);
    for (width, height) in [(1, 1), (2, 2), (10, 3), (20, 5)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        rendered_regions(&mut terminal, &app);
    }
}

/// The top border row as text.
fn top_row(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width)
        .map(|x| buffer[(x, 0)].symbol())
        .collect()
}

#[test]
fn the_interval_and_its_buttons_sit_in_the_top_right_corner() {
    let app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let regions = rendered_regions(&mut terminal, &app);
    let top = top_row(&terminal);

    assert!(top.starts_with("┌ tuxctl "), "{top}");
    assert!(top.ends_with(" ⟳ 1s [-] [+] ┐"), "{top}");
    assert_eq!(regions.interval_buttons.len(), 2);
    for (step, area) in &regions.interval_buttons {
        assert_eq!(area.y, 0);
        for x in area.x..area.right() {
            assert_eq!(
                regions.target_at(x, 0),
                Some(MouseTarget::IntervalStep(*step))
            );
        }
        // The gap after each button is not part of it.
        assert_eq!(regions.target_at(area.right(), 0), None);
    }
    let label = |step| {
        regions
            .interval_buttons
            .iter()
            .find(|(s, _)| *s == step)
            .map(|(_, area)| {
                (area.x..area.right())
                    .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                    .collect::<String>()
            })
    };
    assert_eq!(label(IntervalStep::Shorter).as_deref(), Some("[-]"));
    assert_eq!(label(IntervalStep::Longer).as_deref(), Some("[+]"));
}

#[test]
fn the_system_summary_sits_in_the_top_border_on_the_overview_only() {
    let mut app = App::default();
    let mut metrics = SystemMetrics::default();
    metrics.system_identity.hostname = Some("build-host".into());
    app.update(Action::SystemMetricsUpdated(metrics));
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();

    rendered_regions(&mut terminal, &app);
    let top = top_row(&terminal);
    assert!(top.starts_with("┌ tuxctl · build-host · "), "{top}");
    assert!(top.contains(" zombie ─"), "a gap before the corner: {top}");
    assert!(top.ends_with(" ⟳ 1s [-] [+] ┐"), "{top}");

    app.update(Action::SelectTab(Tab::Processes));
    rendered_regions(&mut terminal, &app);
    assert!(!top_row(&terminal).contains("build-host"));
}

#[test]
fn the_top_right_corner_gives_up_space_without_touching_the_title() {
    let mut app = App::default().with_collector_periods(
        crate::app::CollectorPeriods::for_sampling_interval(std::time::Duration::from_secs(1)),
    );
    for _ in 0..7 {
        app.update(Action::StepSamplingInterval(IntervalStep::Shorter));
    }
    let start = std::time::Instant::now();
    app.update(Action::Tick(start));
    app.update(Action::Tick(start + std::time::Duration::from_secs(10)));

    for width in [40_u16, 30, 24, 20] {
        let mut terminal = Terminal::new(TestBackend::new(width, 15)).unwrap();
        let regions = rendered_regions(&mut terminal, &app);
        if width < layout::MIN_TERMINAL_WIDTH {
            continue;
        }
        let top = top_row(&terminal);
        assert!(top.starts_with("┌ tuxctl "), "{width}: {top}");
        assert!(top.ends_with('┐'), "{width}: {top}");
        assert!(top.contains(" stale "), "{width}: {top}");
        assert!(top.contains("250ms"), "{width}: {top}");
        assert_eq!(regions.interval_buttons.is_empty(), !top.contains("[+]"));
    }
}

#[test]
fn a_modal_disables_the_interval_buttons() {
    let mut app = App::default();
    app.update(Action::ShowHelp);
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let regions = rendered_regions(&mut terminal, &app);

    assert!(regions.interval_buttons.is_empty());
}

/// One of each kind: a CPU package, a GPU at its limit, an asleep disk
/// sensor (`–`) and a NIC.
fn sweep_temperatures() -> Vec<crate::linux::Temperature> {
    use crate::linux::{Temperature, TemperatureKey};
    let device = |path: &str| TemperatureKey::Device(Path::new(path).into());
    vec![
        Temperature {
            key: TemperatureKey::CpuPackage(0),
            celsius: Some(54),
            max: None,
            crit: None,
        },
        Temperature {
            key: device("/sys/devices/gpu"),
            celsius: Some(100),
            max: Some(95),
            crit: None,
        },
        Temperature {
            key: device("/sys/devices/disk"),
            celsius: None,
            max: Some(80),
            crit: None,
        },
        Temperature {
            key: device("/sys/devices/nic"),
            celsius: Some(47),
            max: None,
            crit: None,
        },
    ]
}

fn overview_sweep_app(cpu_count: u32) -> App {
    let mut app = App::default();
    app.update(Action::SystemMetricsUpdated(SystemMetrics {
        cpu_percent: Some(21.0),
        logical_cpus: (0..cpu_count)
            .map(|index| LogicalCpuMetrics {
                id: LogicalCpuId::for_test(index),
                utilization_percent: Some(f64::from(index % 100)),
            })
            .collect(),
        memory: Some(ByteUsage {
            used: 4 << 30,
            total: 16 << 30,
        }),
        mounts: vec![crate::linux::MountUsage {
            mount_point: "/".into(),
            usage: crate::linux::ByteUsage {
                used: 50 << 30,
                total: 100 << 30,
            },
        }],
        temperatures: sweep_temperatures(),
        ..SystemMetrics::default()
    }));
    let module = crate::linux::MemoryModule {
        locator: None,
        capacity_bytes: 8 << 30,
        memory_type: Some("DDR5".into()),
        speed_mts: Some(5600),
        manufacturer: None,
        part_number: None,
    };
    app.update(Action::HardwareDiscovered(
        crate::linux::HardwareInventory {
            cpus: vec![crate::linux::CpuPackage {
                physical_id: Some(0),
                model: "Test Processor".into(),
            }],
            memory_modules: vec![module.clone(), module],
            gpus: vec![crate::linux::GpuDevice {
                model: "Test Graphics".into(),
                kind: None,
                vram_bytes: None,
                device_path: Some(Path::new("/sys/devices/gpu").into()),
                driver: None,
            }],
            storage_devices: vec![crate::linux::StorageDevice {
                system_name: "nvme0n1".into(),
                kind: crate::linux::StorageKind::Nvme,
                model: Some("Test Disk".into()),
                capacity_bytes: Some(1_000_000_000_000),
                device_path: Some(Path::new("/sys/devices/disk").into()),
            }],
            network_devices: vec![crate::linux::NetworkDevice {
                interface_name: "eth0".into(),
                model: None,
                device_path: Some(Path::new("/sys/devices/nic").into()),
            }],
        },
    ));
    app.update(Action::NetworkUpdated(crate::linux::NetworkSnapshot {
        interfaces: vec![crate::linux::NetworkInterfaceInfo {
            name: "eth0".into(),
            operstate: crate::linux::OperState::Up,
            mac_address: None,
            mtu: None,
            ipv4_addresses: Vec::new(),
            ipv6_addresses: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            rx_packets: 0,
            tx_packets: 0,
            rx_errors: 0,
            tx_errors: 0,
            rx_dropped: 0,
            tx_dropped: 0,
            rx_rate_bytes_per_sec: None,
            tx_rate_bytes_per_sec: None,
        }],
        error: None,
    }));
    app
}

/// Rows of the Overview below the tabs, inside the outer border.
fn overview_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width;
    (2..buffer.area.height.saturating_sub(1))
        .map(|y| (1..width - 1).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

/// Every card border that starts below the tabs has a row inside it.
fn cards_have_content(terminal: &Terminal<TestBackend>) -> bool {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    (2..area.height.saturating_sub(1)).all(|y| {
        (1..area.width - 1)
            .all(|x| buffer[(x, y)].symbol() != "┌" || buffer[(x, y + 1)].symbol() != "└")
    })
}

fn shown_cpu_labels(rows: &[String]) -> Vec<u32> {
    let mut labels = Vec::new();
    for row in rows {
        let mut rest = row.as_str();
        while let Some(position) = rest.find("CPU") {
            rest = &rest[position + 3..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(index) = digits.parse() {
                labels.push(index);
            }
        }
    }
    labels
}

fn more_cpus(rows: &[String]) -> usize {
    rows.iter()
        .find_map(|row| {
            let (_, rest) = row.split_once("… ")?;
            let (count, _) = rest.split_once(" more logical CPUs")?;
            count.parse().ok()
        })
        .unwrap_or(0)
}

#[test]
fn overview_resize_sweep_keeps_cpu_counts_and_sections_consistent() {
    for cpu_count in [12, 64] {
        let app = overview_sweep_app(cpu_count);
        for width in [40, 50, 60, 89, 99, 100, 120, 149, 150, 200] {
            let mut previous_shown = 0;
            for height in layout::MIN_TERMINAL_HEIGHT..=60 {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                rendered_regions(&mut terminal, &app);
                let rows = overview_rows(&terminal);
                let context = format!("{cpu_count} CPUs at {width}x{height}:\n{}", rows.join("\n"));
                assert!(
                    cards_have_content(&terminal),
                    "card without content; {context}"
                );

                let shown = shown_cpu_labels(&rows);
                let mut distinct = shown.clone();
                distinct.sort_unstable();
                distinct.dedup();
                assert_eq!(distinct.len(), shown.len(), "{context}");
                // The grid is the first thing a short card gives up; when
                // any of it is shown, it accounts for every CPU.
                let grid_hidden = shown.is_empty() && more_cpus(&rows) == 0;
                assert!(
                    grid_hidden || shown.len() + more_cpus(&rows) == cpu_count as usize,
                    "shown + overflow must equal the CPU count; {context}"
                );
                assert!(
                    shown.len() >= previous_shown,
                    "visible CPUs decreased as height grew; {context}"
                );
                previous_shown = shown.len();
            }
        }
    }
}

#[test]
fn overview_shows_component_temperatures() {
    let app = overview_sweep_app(12);
    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
    rendered_regions(&mut terminal, &app);
    let rows = overview_rows(&terminal);
    let row = |text: &str| {
        rows.iter()
            .find(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text}: {rows:#?}"))
            .clone()
    };

    assert!(row("Test Processor").contains("Test Processor · 54°C"));
    assert!(row("Test Graphics").contains("Test Graphics · 100°C"));
    assert!(row("Test Disk").contains("1 TB  –"), "{}", row("Test Disk"));
    assert!(row("eth0").contains("47°C"));

    // Temperatures take the band of their share of the limit: the GPU is
    // past its driver's 95 °C, the CPU at 54 of an assumed 95 °C.
    let buffer = terminal.backend().buffer();
    let style_of = |text: &str| {
        let (y, line) = buffer
            .content()
            .chunks(160)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .enumerate()
            .find(|(_, line)| line.contains(text))
            .unwrap();
        let x = line[..line.find(text).unwrap()].chars().count();
        buffer[(x as u16, y as u16)].fg
    };
    assert_eq!(style_of("100°C"), ratatui::style::Color::Red);
    assert_eq!(style_of("54°C"), ratatui::style::Color::Green);
}

#[test]
fn hardware_panel_with_temperatures_survives_tiny_areas() {
    let app = overview_sweep_app(12);
    for width in 0..=60 {
        for height in 0..=40 {
            let mut terminal =
                Terminal::new(TestBackend::new(width.max(1), height.max(1))).unwrap();
            terminal
                .draw(|frame| {
                    let area = Rect::new(
                        0,
                        0,
                        width.min(frame.area().width),
                        height.min(frame.area().height),
                    );
                    overview::render(frame, &app, area);
                })
                .unwrap();
        }
    }
}

#[test]
fn dense_cpu_cells_keep_a_separator_after_the_label() {
    let app = overview_sweep_app(64);
    for height in layout::MIN_TERMINAL_HEIGHT..=60 {
        let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
        rendered_regions(&mut terminal, &app);
        for row in overview_rows(&terminal) {
            let mut rest = row.as_str();
            while let Some(position) = rest.find("CPU") {
                rest = &rest[position + 3..];
                let after_digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                if after_digits.len() != rest.len() {
                    assert!(
                        after_digits.starts_with(' ') || after_digits.is_empty(),
                        "CPU label runs into its value at 40x{height}: {row}"
                    );
                }
            }
        }
    }
}

#[test]
fn tab_labels_never_touch_and_hit_regions_match_labels() {
    let app = App::default();
    for width in 40..=130 {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        let regions = rendered_regions(&mut terminal, &app);
        let tab_row: Vec<String> = terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .nth(1)
            .unwrap()
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect();

        assert_eq!(regions.tabs.len(), Tab::ALL.len(), "at width {width}");
        for pair in regions.tabs.windows(2) {
            let (left, right) = (pair[0].area, pair[1].area);
            let left_label = tab_row[usize::from(left.x)..usize::from(left.right())].concat();
            let right_label = tab_row[usize::from(right.x)..usize::from(right.right())].concat();
            assert!(
                left_label.ends_with(' ') || right_label.starts_with(' ') || right.x > left.right(),
                "tab labels touch at width {width}: {left_label:?} {right_label:?}"
            );
        }
        for region in &regions.tabs {
            let label =
                tab_row[usize::from(region.area.x)..usize::from(region.area.right())].concat();
            assert!(
                !label.trim().is_empty(),
                "empty tab hit region at width {width}"
            );
            assert_eq!(
                regions.target_at(region.area.x, region.area.y),
                Some(MouseTarget::Tab(region.tab))
            );
        }
    }
}

/// The rendered row containing `persistent` (the search or filter text).
fn status_row(app: &App, width: u16, persistent: &str) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
    rendered_regions(&mut terminal, app);
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .find(|row| row.contains(persistent))
        .unwrap_or_else(|| panic!("{persistent:?} is not visible at width {width}"))
}

fn process_info(pid: u32, name: &str) -> crate::linux::ProcessInfo {
    crate::linux::ProcessInfo {
        pid,
        name: name.into(),
        cpu_percent: None,
        memory_bytes: 0,
        command: None,
        state: "S".into(),
        parent_pid: 1,
        state_code: 'S',
        start_time: u64::from(pid),
        kernel_thread: false,
    }
}

fn process_snapshot(processes: Vec<crate::linux::ProcessInfo>) -> Action {
    Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
        processes,
        error: None,
    })
}

/// Processes tab with a committed filter "ss" (typed, detail opened and closed).
fn filtered_processes_app() -> App {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    app.update(process_snapshot(vec![
        process_info(1, "sshd"),
        process_info(2, "init"),
    ]));
    app.update(Action::BeginProcessSearch);
    app.update(Action::AppendProcessSearch('s'));
    app.update(Action::AppendProcessSearch('s'));
    app.update(Action::OpenProcessDetails);
    app.update(Action::Escape);
    assert_eq!(app.input_mode(), InputMode::Normal);
    app
}

#[test]
fn process_filter_stays_visible_with_an_action_message() {
    let mut app = filtered_processes_app();
    app.update(Action::RequestProcessSignal(
        crate::linux::ProcessSignal::Term,
    ));
    app.update(process_snapshot(vec![process_info(2, "init")]));
    assert!(app.process_action_message().is_some());

    status_row(&app, 40, "Filter: \"ss\"");
    let row = status_row(&app, 120, "Filter: \"ss\"");
    assert!(row.contains("exited before signal"), "{row}");
}

#[test]
fn process_filter_stays_visible_with_a_refresh_error() {
    let mut app = filtered_processes_app();
    app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
        processes: Vec::new(),
        error: Some("proc unavailable".into()),
    }));

    status_row(&app, 40, "Filter: \"ss\"");
    let row = status_row(&app, 120, "Filter: \"ss\"");
    assert!(row.contains("Refresh error: proc unavailable"), "{row}");
}

#[test]
fn service_search_and_filter_stay_visible_while_refreshing_or_failing() {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Services));
    assert!(app.service_refreshing());
    app.update(Action::BeginServiceSearch);
    app.update(Action::AppendServiceSearch('x'));
    status_row(&app, 40, "Search: x_");
    let row = status_row(&app, 120, "Search: x_");
    assert!(row.contains("Refreshing system services"), "{row}");

    let generation = app.take_service_refresh_request().unwrap();
    app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
        services: vec![crate::linux::ServiceInfo {
            unit: "xyz.service".into(),
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            description: "X".into(),
        }],
        error: None,
        completed_refresh_generation: generation,
    }));
    app.update(Action::OpenServiceDetails);
    app.update(Action::Escape);
    app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
        error: Some("bus unavailable".into()),
        ..Default::default()
    }));
    status_row(&app, 40, "Filter: \"x\"");
    let row = status_row(&app, 120, "Filter: \"x\"");
    assert!(row.contains("Refresh error: bus unavailable"), "{row}");
}

#[test]
fn logs_header_shows_the_full_priority_label() {
    let app = populated_app(Tab::Logs);
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    rendered_regions(&mut terminal, &app);

    let text = buffer_text(&terminal);
    assert!(text.contains("PRIORITY"), "priority header was truncated");
    assert!(text.contains("warning"));
}

#[test]
fn overview_filesystem_row_keeps_label_and_bar_separate() {
    let app = populated_app(Tab::Overview);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    rendered_regions(&mut terminal, &app);

    let lines: Vec<String> = terminal
        .backend()
        .buffer()
        .content()
        .chunks(120)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect();
    let row = lines
        .iter()
        .find(|line| line.contains("/   50%"))
        .expect("filesystem row in the Storage card");
    assert!(row.contains("/   50%  █"), "row: {row}");
    assert!(row.contains("░  50.0 GiB / 100.0 GiB"), "row: {row}");
}

#[test]
fn loopback_unknown_state_renders_neutrally() {
    let app = populated_app(Tab::Network);
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    rendered_regions(&mut terminal, &app);

    let text = buffer_text(&terminal);
    assert!(text.contains("◌ unknown"));
    assert!(!text.contains("UNKNOWN"));
}

#[test]
fn every_tab_renders_populated_data_at_the_minimum_size() {
    for tab in [
        Tab::Overview,
        Tab::Processes,
        Tab::Services,
        Tab::Logs,
        Tab::Network,
    ] {
        let app = populated_app(tab);
        for (width, height) in [
            (layout::MIN_TERMINAL_WIDTH, layout::MIN_TERMINAL_HEIGHT),
            (
                layout::MIN_TERMINAL_WIDTH + 1,
                layout::MIN_TERMINAL_HEIGHT + 1,
            ),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let regions = rendered_regions(&mut terminal, &app);
            assert!(!regions.tabs.is_empty(), "{tab:?} at {width}x{height}");
        }
    }
}

#[test]
fn implemented_screens_use_the_warning_path_in_tiny_terminals() {
    for tab in [
        Tab::Overview,
        Tab::Processes,
        Tab::Services,
        Tab::Logs,
        Tab::Network,
    ] {
        let mut app = App::default();
        app.update(Action::SelectTab(tab));

        for (width, height) in [(1, 1), (2, 2), (10, 3)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            assert!(rendered_regions_are_empty(&mut terminal, &app));
        }
    }
}

fn rendered_regions_are_empty(terminal: &mut Terminal<TestBackend>, app: &App) -> bool {
    let mut empty = false;
    terminal
        .draw(|frame| {
            empty = render(frame, app).tabs.is_empty();
        })
        .unwrap();
    empty
}

#[test]
fn minimum_terminal_size_render_boundary_is_global() {
    let app = App::default();
    for (width, height) in [(39, 15), (40, 14), (39, 14)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        assert!(rendered_regions_are_empty(&mut terminal, &app));
        let text = buffer_text(&terminal);
        assert!(text.contains("Terminal too small"));
        assert!(text.contains(&format!("Current: {width}x{height}")));
    }

    let backend = TestBackend::new(40, 15);
    let mut terminal = Terminal::new(backend).unwrap();
    assert!(!rendered_regions_are_empty(&mut terminal, &app));
    assert!(!buffer_text(&terminal).contains("Terminal too small"));
}

#[test]
fn minimum_terminal_warning_is_safe_at_pathological_sizes() {
    let app = App::default();
    for (width, height) in [(1, 1), (1, 40), (40, 1), (2, 2)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        assert!(rendered_regions_are_empty(&mut terminal, &app));
    }
}

#[test]
fn resize_transitions_clear_warning_and_normal_content() {
    let app = App::default();
    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    assert!(!rendered_regions_are_empty(&mut terminal, &app));
    assert!(buffer_text(&terminal).contains("Processes"));

    terminal.backend_mut().resize(39, 40);
    terminal.autoresize().unwrap();
    assert!(rendered_regions_are_empty(&mut terminal, &app));
    let warning = buffer_text(&terminal);
    assert!(warning.contains("Terminal too small"));
    assert!(!warning.contains("Processes"));

    terminal.backend_mut().resize(80, 40);
    terminal.autoresize().unwrap();
    assert!(!rendered_regions_are_empty(&mut terminal, &app));
    let normal = buffer_text(&terminal);
    assert!(normal.contains("Processes"));
    assert!(!normal.contains("Terminal too small"));
}

#[test]
fn process_confirmation_regions_are_rebuilt_after_resize() {
    let app = process_confirmation_app();
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let old_regions = rendered_regions(&mut terminal, &app);
    let old_confirm = old_regions.process_signal_confirm.unwrap();
    let old_viewport = old_regions.process_viewport();

    terminal.backend_mut().resize(50, 16);
    terminal.autoresize().unwrap();
    let new_regions = rendered_regions(&mut terminal, &app);
    let new_cancel = new_regions.process_signal_cancel.unwrap();
    let new_confirm = new_regions.process_signal_confirm.unwrap();

    assert_ne!(new_confirm, old_confirm);
    assert_ne!(new_regions.process_viewport(), old_viewport);
    assert_eq!(
        new_regions.target_at(new_cancel.x, new_cancel.y),
        Some(MouseTarget::ProcessSignalCancel)
    );
    assert_eq!(
        new_regions.target_at(new_confirm.x, new_confirm.y),
        Some(MouseTarget::ProcessSignalConfirm)
    );
    assert_eq!(
        new_regions.target_at(old_confirm.x, old_confirm.y),
        None,
        "the prior confirmation coordinates must not remain active"
    );
    assert!(new_regions.tabs.is_empty());
    assert!(new_regions.process_rows.is_empty());
    assert!(new_regions.process_headers.is_empty());
    assert!(new_regions.process_scroll_area.is_none());
}

#[test]
fn modal_resize_crosses_each_minimum_terminal_boundary() {
    let mut app = process_confirmation_app();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    for (width, height) in [
        (layout::MIN_TERMINAL_WIDTH - 1, layout::MIN_TERMINAL_HEIGHT),
        (layout::MIN_TERMINAL_WIDTH, layout::MIN_TERMINAL_HEIGHT - 1),
        (
            layout::MIN_TERMINAL_WIDTH - 1,
            layout::MIN_TERMINAL_HEIGHT - 1,
        ),
    ] {
        let valid_regions = rendered_regions(&mut terminal, &app);
        assert!(valid_regions.process_signal_confirm.is_some());
        assert!(buffer_text(&terminal).contains("Terminate Process"));

        terminal.backend_mut().resize(width, height);
        terminal.autoresize().unwrap();
        assert!(app.update(Action::Resize));
        assert!(app.process_signal_confirmation().is_some());
        let small_regions = rendered_regions(&mut terminal, &app);
        assert!(small_regions.process_signal_confirm.is_none());
        assert!(small_regions.tabs.is_empty());
        assert!(buffer_text(&terminal).contains("Terminal too small"));

        terminal.backend_mut().resize(80, 24);
        terminal.autoresize().unwrap();
        assert!(app.update(Action::Resize));
        assert!(app.process_signal_confirmation().is_some());
        let restored_regions = rendered_regions(&mut terminal, &app);
        assert!(restored_regions.process_signal_confirm.is_some());
        assert!(restored_regions.tabs.is_empty());
        let restored = buffer_text(&terminal);
        assert!(restored.contains("Terminate Process"));
        assert!(!restored.contains("Terminal too small"));
    }
}

#[test]
fn overview_dashboard_renders_cached_data_across_responsive_sizes() {
    let mut app = App::default();
    let logical_cpus = (0..64)
        .map(|index| LogicalCpuMetrics {
            id: LogicalCpuId::for_test(index),
            utilization_percent: Some(f64::from(index % 101)),
        })
        .collect::<Vec<_>>();

    for sample in 0..65 {
        let mut metrics = SystemMetrics {
            cpu_percent: Some(f64::from(sample)),
            logical_cpus: logical_cpus.clone(),
            memory: Some(ByteUsage {
                used: 8 * 1024 * 1024 * 1024,
                total: 32 * 1024 * 1024 * 1024,
            }),
            uptime: Some(std::time::Duration::from_secs(90_000)),
            mounts: vec![crate::linux::MountUsage {
                mount_point: "/".into(),
                usage: crate::linux::ByteUsage {
                    used: 120 * 1024 * 1024 * 1024,
                    total: 500 * 1024 * 1024 * 1024,
                },
            }],
            ..SystemMetrics::default()
        };
        metrics.system_identity.hostname = Some("build-host".into());
        metrics.system_identity.kernel_release = Some("6.12.0-tuxctl".into());
        app.update(Action::SystemMetricsUpdated(metrics));
    }

    for (width, height) in [
        (180, 50),
        (120, 40),
        (90, 40),
        (80, 40),
        (46, 50),
        (46, 60),
        (40, 40),
        (40, 15),
    ] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &app);
            })
            .unwrap();

        let buffer = buffer_text(&terminal);
        assert!(!buffer.contains("Dashboard"));

        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();

        // Row 0 is the outer border with the system summary, row 1 the
        // tabs, row 2 the top border of the first card directly below them.
        let context = format!("{width}x{height}:\n{}", rows.join("\n"));
        assert!(rows[1].contains("Overview") || rows[1].contains(" Ovr "));
        if width >= 60 {
            assert!(rows[0].contains("· build-host · "), "{context}");
        }
        assert!(rows[2].contains(" CPU "), "{context}");
        assert!(!rows[2].contains("Overview"));
        assert!(!rows[3].trim().is_empty());

        if width - 2 >= overview::MEDIUM_MIN_WIDTH {
            assert!(rows[2].contains(" GPU "), "{context}");
        } else {
            assert!(
                rows.iter().skip(3).any(|row| row.contains(" Memory ")),
                "{context}"
            );
        }
    }
}

#[test]
fn data_screens_start_with_meaningful_content_below_tabs_at_responsive_sizes() {
    for (tab, marker) in [
        (Tab::Processes, "0 processes"),
        (Tab::Services, "0 services"),
        (Tab::Logs, "0 entries"),
        (Tab::Network, "0 interfaces"),
    ] {
        let mut app = App::default();
        app.update(Action::SelectTab(tab));
        if let Some(generation) = app.take_service_refresh_request() {
            // Settle the refresh that entering Services requests.
            app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
                completed_refresh_generation: generation,
                ..Default::default()
            }));
        }

        for (width, height) in [(40, 15), (46, 20), (80, 24), (120, 40), (180, 50)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let regions = rendered_regions(&mut terminal, &app);
            let rows = terminal
                .backend()
                .buffer()
                .content()
                .chunks(usize::from(width))
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();

            assert!(rows[1].contains(tab.label()) || rows[1].contains(tab.short_label()));
            assert!(
                rows[2].contains(marker),
                "{tab:?} at {width}x{height} did not start with {marker:?}"
            );
            assert!(!rows[2].contains("Terminal too small"));

            let viewport = match tab {
                Tab::Processes => regions.process_viewport(),
                Tab::Services => regions.service_viewport(),
                Tab::Logs => regions.log_viewport(),
                Tab::Network => regions.network_viewport(),
                Tab::Overview => unreachable!(),
            };
            assert!(viewport.is_some_and(|(_, height)| height > 0));
        }
    }
}

#[test]
fn removing_redundant_screen_titles_reclaims_a_table_row() {
    let mut processes = App::default();
    processes.update(Action::SelectTab(Tab::Processes));
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let regions = rendered_regions(&mut terminal, &processes);

    assert_eq!(regions.process_viewport(), Some((0, 18)));
    assert_eq!(regions.process_scroll_area.unwrap().y, 5);

    let mut services = App::default();
    services.update(Action::SelectTab(Tab::Services));
    let regions = rendered_regions(&mut terminal, &services);

    assert_eq!(regions.service_viewport(), Some((0, 19)));
    assert_eq!(regions.service_scroll_area.unwrap().y, 4);
}

fn pinned_processes_app(pins: &[u32]) -> App {
    let mut app = App::default();
    app.update(Action::SelectTab(Tab::Processes));
    app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
        processes: (1..=20)
            .map(|pid| crate::linux::ProcessInfo {
                pid,
                name: format!("proc{pid}"),
                cpu_percent: Some(f64::from(pid)),
                memory_bytes: 1 << 30,
                command: None,
                state: "S (sleeping)".into(),
                parent_pid: 1,
                state_code: 'S',
                start_time: u64::from(pid),
                kernel_thread: false,
            })
            .collect(),
        error: None,
    }));
    for &pid in pins {
        app.update(Action::SelectProcess(pinned(pid)));
        app.update(Action::TogglePin);
    }
    app
}

fn pinned(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_time: u64::from(pid),
    }
}

#[test]
fn pin_controls_move_their_row_and_hover_as_the_row() {
    let app = pinned_processes_app(&[3, 5, 7]);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let regions = rendered_regions(&mut terminal, &app);
    let row = |pid| {
        regions
            .process_rows
            .iter()
            .find(|region| region.identity == pinned(pid))
            .unwrap()
    };

    // First pin: down only; middle: both; last: up only; unpinned: none.
    assert!(row(3).pin_up.is_none() && row(3).pin_down.is_some());
    assert!(row(5).pin_up.is_some() && row(5).pin_down.is_some());
    assert!(row(7).pin_up.is_some() && row(7).pin_down.is_none());
    assert!(row(20).pin_up.is_none() && row(20).pin_down.is_none());

    let up = row(5).pin_up.unwrap();
    let down = row(5).pin_down.unwrap();
    assert!(contains(row(5).area, up.x, up.y) && contains(row(5).area, down.x, down.y));
    assert_eq!(
        regions.target_at(up.x, up.y),
        Some(MouseTarget::PinMove(pinned(5), PinMove::Up))
    );
    assert_eq!(
        regions.target_at(down.x + 1, down.y),
        Some(MouseTarget::PinMove(pinned(5), PinMove::Down))
    );
    assert_eq!(
        regions.target_at(up.x - 1, up.y),
        Some(MouseTarget::ProcessRow(pinned(5))),
        "one cell left of the control is the row"
    );
    // Hovering a control is hovering its row: no extra hover transitions.
    for x in [up.x, down.x, row(5).area.x] {
        assert_eq!(
            regions.hover_target_at(x, up.y),
            Some(MouseTarget::ProcessRow(pinned(5)))
        );
    }

    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(up.x, up.y)].symbol(), "▲");
    assert_eq!(buffer[(down.x, down.y)].symbol(), "▼");
    assert_eq!(
        rows_text(&terminal, &regions).matches(['▲', '▼']).count(),
        4
    );
}

/// Text of the process table body (without the sortable header).
fn rows_text(terminal: &Terminal<TestBackend>, regions: &UiRegions) -> String {
    let buffer = terminal.backend().buffer();
    regions
        .process_rows
        .iter()
        .flat_map(|region| (region.area.x..region.area.right()).map(move |x| (x, region.area.y)))
        .map(|position| buffer[position].symbol())
        .collect()
}

#[test]
fn pin_controls_need_two_pins_and_room() {
    for (pins, width) in [(&[3_u32][..], 100), (&[3, 5][..], 60)] {
        let app = pinned_processes_app(pins);
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        let regions = rendered_regions(&mut terminal, &app);

        assert!(
            regions
                .process_rows
                .iter()
                .all(|region| region.pin_up.is_none() && region.pin_down.is_none()),
            "{pins:?} at {width} columns"
        );
        assert!(!rows_text(&terminal, &regions).contains(['▲', '▼']));
    }
}

#[test]
fn a_modal_removes_pin_controls() {
    let mut app = pinned_processes_app(&[3, 5]);
    app.update(Action::RequestProcessSignal(
        crate::linux::ProcessSignal::Term,
    ));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let regions = rendered_regions(&mut terminal, &app);

    assert!(regions.process_rows.is_empty());
}

#[test]
fn menu_items_are_hit_from_their_rendered_rows_and_hide_the_background() {
    let mut app = populated_app(Tab::Processes);
    app.update(Action::Escape);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let regions = rendered_regions(&mut terminal, &app);

    assert!(regions.tabs.is_empty() && regions.process_rows.is_empty());
    assert_eq!(regions.menu_items.len(), 2);
    for (item, area) in &regions.menu_items {
        assert_eq!(
            regions.target_at(area.x, area.y),
            Some(MouseTarget::MenuItem(*item))
        );
        assert_eq!(regions.target_at(area.x, area.y + 5), None);
    }
    let text = buffer_text(&terminal);
    assert!(text.contains("About") && text.contains("Exit"));
}

#[test]
fn menu_and_about_render_at_every_size() {
    let mut app = App::default();
    app.update(Action::Escape);
    let mut about = App::default();
    about.update(Action::Escape);
    about.update(Action::ActivateSelectedMenuItem);
    for (width, height) in [(40, 15), (40, 5), (1, 1), (200, 60)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        rendered_regions(&mut terminal, &app);
        rendered_regions(&mut terminal, &about);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    rendered_regions(&mut terminal, &about);
    let text = buffer_text(&terminal);
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
    assert!(text.contains(env!("CARGO_PKG_RUST_VERSION")));
}

#[test]
fn help_lists_every_binding_and_every_line_fits() {
    for nvidia in [
        NvidiaAccess::On,
        NvidiaAccess::Off,
        NvidiaAccess::Unsupported,
    ] {
        for line in help_lines(nvidia) {
            assert!(
                line.to_string().chars().count() <= usize::from(HELP_WIDTH - 2),
                "{line:?} is cut off"
            );
        }
    }
    let lines = help_lines(NvidiaAccess::Off);
    let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
    for line in &text {
        assert!(
            line.chars().count() <= usize::from(HELP_WIDTH - 2),
            "{line:?} is cut off"
        );
    }
    let all = text.join("\n");
    for binding in [
        "+ / -",
        "Esc",
        "menu",
        "↑/↓ move while typing",
        "P pin",
        "Shift+↑/↓",
        "▲/▼",
        "v kernel threads",
        "Ctrl+C",
        "Space",
        "NVIDIA GPUs",
    ] {
        assert!(all.contains(binding), "Help does not mention {binding:?}");
    }

    // Complete, including the closing hint, once the terminal is tall enough.
    let mut app = App::default();
    app.update(Action::ShowHelp);
    let mut terminal = Terminal::new(TestBackend::new(80, lines.len() as u16 + 4)).unwrap();
    rendered_regions(&mut terminal, &app);
    assert!(buffer_text(&terminal).contains("Esc closes"));
    for (width, height) in [(40, 15), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        rendered_regions(&mut terminal, &app);
    }
}

#[test]
fn process_signal_button_hit_testing() {
    let regions =
        UiRegions::from_process_signal_buttons(Rect::new(10, 5, 12, 1), Rect::new(26, 5, 15, 1));

    assert_eq!(regions.target_at(9, 5), None);
    assert_eq!(
        regions.target_at(10, 5),
        Some(MouseTarget::ProcessSignalCancel)
    );
    assert_eq!(
        regions.target_at(21, 5),
        Some(MouseTarget::ProcessSignalCancel)
    );
    assert_eq!(regions.target_at(22, 5), None);

    assert_eq!(
        regions.target_at(26, 5),
        Some(MouseTarget::ProcessSignalConfirm)
    );
    assert_eq!(
        regions.target_at(40, 5),
        Some(MouseTarget::ProcessSignalConfirm)
    );
    assert_eq!(regions.target_at(41, 5), None);
    assert_eq!(regions.target_at(10, 6), None);
}

#[test]
fn process_signal_modal_renders_in_all_terminal_sizes() {
    for signal in [
        crate::linux::ProcessSignal::Term,
        crate::linux::ProcessSignal::Kill,
    ] {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes: vec![crate::linux::ProcessInfo {
                pid: 1234,
                name: "testproc".into(),
                cpu_percent: Some(5.0),
                memory_bytes: 4096,
                command: Some("/bin/testproc".into()),
                state: "R (running)".into(),
                parent_pid: 1,
                state_code: 'R',
                start_time: 100,
                kernel_thread: false,
            }],
            error: None,
        }));
        app.update(Action::RequestProcessSignal(signal));

        for (width, height) in [(1, 1), (5, 5), (20, 8), (60, 15), (120, 40)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, &app);
                })
                .unwrap();
        }
    }
}

#[test]
fn tabs_hint_includes_horizontal_offset() {
    let app = App::default();
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            render(frame, &app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let line_chars = (81..99)
        .map(|x| buffer[(x, 1)].symbol())
        .collect::<String>();
    assert_eq!(line_chars, "1-5 Tabs   ? Help ");
}
