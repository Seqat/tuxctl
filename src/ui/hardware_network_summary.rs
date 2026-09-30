use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{overview_interfaces, App},
    linux::{NetworkInterfaceInfo, OperState, Temperature},
};

use super::{
    cards::{self, CardGraph, CardTitle, TitleOrder},
    hardware::{device_temperature, temperature_span, NIC_DEFAULT_LIMIT},
    layout, network,
    overview::CardHeight,
};

/// Interfaces listed under the primary one when there is room.
const MAX_OTHER_INTERFACES: u16 = 3;

fn other_interface_rows(app: &App) -> u16 {
    let count = overview_interfaces(app.networks(), app.hardware()).len();
    (count.saturating_sub(1) as u16).min(MAX_OTHER_INTERFACES)
}

pub(super) fn card_height(app: &App, graphs: bool) -> CardHeight {
    CardHeight::new(1, other_interface_rows(app), graphs)
}

/// Network: the primary interface with its state and temperature in the
/// title, a graph of the traffic of the listed interfaces, the primary
/// interface's rates, then one row per other interface.
pub(super) fn render_card(frame: &mut Frame, app: &App, area: Rect, graphs: bool) {
    let inventory = app.hardware();
    let interfaces = if app.network_error().is_some() {
        Vec::new()
    } else {
        overview_interfaces(app.networks(), inventory)
    };
    let device = |interface: &NetworkInterfaceInfo| {
        inventory.and_then(|inventory| {
            inventory
                .network_devices
                .iter()
                .find(|device| device.interface_name == interface.name)
        })
    };
    let temperature = |interface: &NetworkInterfaceInfo| {
        device(interface).and_then(|device| {
            device_temperature(app.system_metrics(), device.device_path.as_ref())
        })
    };
    let primary = interfaces.first().copied();
    let mut parts = Vec::new();
    if let Some(interface) = primary {
        let (state, style) = network::state_display(interface.operstate);
        parts.push(vec![Span::styled(state, style)]);
        if let Some(temperature) = temperature(interface) {
            parts.push(vec![temperature_span(temperature, NIC_DEFAULT_LIMIT)]);
        }
    }
    let title = CardTitle {
        name: "Network",
        model: primary.map_or("", |interface| interface.name.as_str()),
        parts,
        order: TitleOrder::PartsFirst,
    };
    let history = app.network_history();
    let peak = history.iter().fold(0.0_f64, f64::max);
    let graph = CardGraph {
        history,
        scale: peak.max(1.0),
        interval: app.cpu_history_interval(),
        banded: false,
        // One KiB/s and less stays at the baseline; a spike leaves the
        // everyday traffic readable.
        log_unit: Some(1024.0),
    };
    let (rows, other_limit) = cards::render_graph_card(
        frame,
        area,
        &title,
        Some(graph),
        graphs,
        1,
        other_interface_rows(app),
    );
    if rows.width == 0 || rows.height == 0 {
        return;
    }
    let width = usize::from(rows.width);
    let lines = if app.network_error().is_some() {
        vec![Line::from("Network data unavailable")]
    } else if app.networks().is_empty() {
        vec![Line::from("No interfaces found")]
    } else if let Some(primary) = primary {
        let peak = (history.iter().len() > 0).then_some(peak);
        let model = device(primary).and_then(|device| device.model.as_deref());
        let mut lines = vec![Line::from(rates_line(primary, peak, model, width))];
        let others = &interfaces[1..];
        let overflow = others.len() > usize::from(other_limit);
        let shown = usize::from(other_limit.saturating_sub(u16::from(overflow))).min(others.len());
        lines.extend(others[..shown].iter().map(|interface| {
            let model = device(interface).and_then(|device| device.model.as_deref());
            network_summary_line(interface, model, temperature(interface), width)
        }));
        if overflow && other_limit > 0 {
            lines.push(Line::from(format!(
                "… {} more interfaces",
                others.len() - shown
            )));
        }
        lines
    } else {
        vec![Line::from("No hardware interfaces found")]
    };
    frame.render_widget(Paragraph::new(lines), rows);
}

/// `RX 1.2 MiB/s  TX 84.2 KiB/s  peak 2.0 MiB/s`, followed by the adapter
/// model when it fits; tighter forms on narrow cards.
fn rates_line(
    interface: &NetworkInterfaceInfo,
    peak: Option<f64>,
    model: Option<&str>,
    width: usize,
) -> String {
    let (rx, tx) = (
        interface.rx_rate_bytes_per_sec,
        interface.tx_rate_bytes_per_sec,
    );
    let full_peak = peak
        .map(|peak| format!("  peak {}", network::format_rate(Some(peak))))
        .unwrap_or_default();
    let tight_peak = peak
        .map(|peak| format!(" ▲{}", format_rate_tight(Some(peak))))
        .unwrap_or_default();
    let candidates = [
        format!(
            "RX {}  TX {}{full_peak}",
            network::format_rate(rx),
            network::format_rate(tx)
        ),
        format!(
            "RX {}  TX {}",
            network::format_rate(rx),
            network::format_rate(tx)
        ),
        format!(
            "R{} T{}{tight_peak}",
            format_rate_tight(rx),
            format_rate_tight(tx)
        ),
        format!("R{} T{}", format_rate_tight(rx), format_rate_tight(tx)),
    ];
    let line = candidates
        .iter()
        .find(|line| line.chars().count() <= width)
        .cloned()
        .unwrap_or_else(|| layout::truncate(&candidates[3], width));
    match model {
        Some(model) => {
            let room = width.saturating_sub(line.chars().count() + 2);
            if room >= 8 {
                format!("{line}  {}", layout::truncate(model, room))
            } else {
                line
            }
        }
        None => line,
    }
}

fn network_summary_line(
    interface: &NetworkInterfaceInfo,
    model: Option<&str>,
    temperature: Option<&Temperature>,
    width: usize,
) -> Line<'static> {
    let (prefix, state, suffix, temperature) =
        network_summary_parts(interface, model, temperature, width);
    let (_, state_style) = network::state_display(interface.operstate);
    let mut spans = vec![
        Span::raw(prefix),
        Span::styled(state, state_style),
        Span::raw(suffix),
    ];
    if let Some(temperature) = temperature {
        spans.extend([Span::raw("  "), temperature]);
    }
    Line::from(spans)
}

/// Name, state, traffic and temperature, fitted in that order of priority;
/// the model takes whatever room is left.
fn network_summary_parts(
    interface: &NetworkInterfaceInfo,
    model: Option<&str>,
    temperature: Option<&Temperature>,
    width: usize,
) -> (String, String, String, Option<Span<'static>>) {
    let (state_text, _) = network::state_display(interface.operstate);
    let state = layout::truncate(state_text, width);
    let separator_width = usize::from(width > state.chars().count()) * 2;
    let name_width = width
        .saturating_sub(state.chars().count() + separator_width)
        .min(16);
    let name = layout::truncate(&interface.name, name_width);
    let mut prefix = if name.is_empty() {
        String::new()
    } else {
        format!("{name}  ")
    };

    let traffic = network_traffic(interface).and_then(|traffic| {
        let full = format!("  RX {}  TX {}", traffic.0, traffic.1);
        let compact = format!("  R {}  T {}", traffic.0, traffic.1);
        let tight = format!(
            "  R{} T{}",
            format_rate_tight(interface.rx_rate_bytes_per_sec),
            format_rate_tight(interface.tx_rate_bytes_per_sec)
        );
        let base_width = prefix.chars().count() + state.chars().count();
        if base_width + full.chars().count() <= width {
            Some(full)
        } else if base_width + compact.chars().count() <= width {
            Some(compact)
        } else if base_width + tight.chars().count() <= width {
            Some(tight)
        } else {
            None
        }
    });
    let suffix = traffic.unwrap_or_default();
    let occupied = prefix.chars().count() + state.chars().count() + suffix.chars().count();
    let temperature = temperature
        .map(|temperature| temperature_span(temperature, NIC_DEFAULT_LIMIT))
        .filter(|span| occupied + 2 + span.content.chars().count() <= width);
    let occupied = occupied
        + temperature
            .as_ref()
            .map_or(0, |span| 2 + span.content.chars().count());

    if let Some(model) = model {
        let model_width = width.saturating_sub(occupied + 2);
        if model_width >= 4 {
            prefix.push_str(&layout::truncate(model, model_width));
            prefix.push_str("  ");
        }
    }

    (prefix, state, suffix, temperature)
}

fn network_traffic(interface: &NetworkInterfaceInfo) -> Option<(String, String)> {
    let has_traffic = [
        interface.rx_rate_bytes_per_sec,
        interface.tx_rate_bytes_per_sec,
    ]
    .into_iter()
    .flatten()
    .any(|rate| rate >= 1.0);
    if interface.operstate != OperState::Up && !has_traffic {
        return None;
    }
    Some((
        network::format_rate(interface.rx_rate_bytes_per_sec),
        network::format_rate(interface.tx_rate_bytes_per_sec),
    ))
}

pub(super) fn format_rate_tight(rate: Option<f64>) -> String {
    const UNITS: [&str; 5] = ["B/s", "K/s", "M/s", "G/s", "T/s"];
    let Some(mut value) = rate else {
        return "--".into();
    };
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 10.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::linux::HardwareInventory;

    fn test_interface(name: &str, state: OperState) -> NetworkInterfaceInfo {
        NetworkInterfaceInfo {
            name: name.into(),
            operstate: state,
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
            rx_rate_bytes_per_sec: Some(1.2 * 1024.0 * 1024.0),
            tx_rate_bytes_per_sec: Some(84.2 * 1024.0),
        }
    }

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn network_summary_handles_models_rates_and_down_interfaces() {
        let up = test_interface("enp8s0", OperState::Up);
        let with_model = network_summary_parts(&up, Some("Realtek RTL8125 2.5GbE"), None, 100);
        let with_model = format!("{}{}{}", with_model.0, with_model.1, with_model.2);
        assert!(with_model.contains("enp8s0  Realtek RTL8125 2.5GbE  ● up"));
        assert!(with_model.contains("RX 1.2 MiB/s"));
        assert!(with_model.contains("TX 84.2 KiB/s"));

        let without_model = network_summary_parts(&up, None, None, 100);
        let without_model = format!("{}{}{}", without_model.0, without_model.1, without_model.2);
        assert!(without_model.starts_with("enp8s0  ● up"));

        let mut down = test_interface("wlp5s0", OperState::Down);
        down.rx_rate_bytes_per_sec = Some(0.0);
        down.tx_rate_bytes_per_sec = None;
        let down = network_summary_parts(&down, Some("Intel Wi-Fi 6E AX210"), None, 80);
        let down = format!("{}{}{}", down.0, down.1, down.2);
        assert!(down.contains("○ down"));
        assert!(!down.contains("RX"));
        assert!(!down.contains("TX"));
    }

    #[test]
    fn network_summary_truncates_long_models_to_available_width() {
        let interface = test_interface("enp8s0", OperState::Up);
        let parts = network_summary_parts(
            &interface,
            Some("A deliberately very long network adapter model description"),
            None,
            60,
        );
        let text = format!("{}{}{}", parts.0, parts.1, parts.2);

        assert!(text.chars().count() <= 60);
        assert!(text.contains('…'));
        assert!(text.contains("● up"));
        assert!(text.contains("RX"));
    }

    #[test]
    fn network_summary_prefers_physical_interfaces_and_filters_noise() {
        let interfaces = [
            test_interface("docker0", OperState::Up),
            test_interface("wlp5s0", OperState::Down),
            test_interface("veth1234", OperState::Up),
            test_interface("enp6s0", OperState::Up),
            test_interface("lo", OperState::Unknown),
        ];
        let inventory = HardwareInventory {
            network_devices: vec![crate::linux::NetworkDevice {
                interface_name: "enp6s0".into(),
                model: None,
                device_path: None,
            }],
            ..HardwareInventory::default()
        };

        let selected = overview_interfaces(&interfaces, Some(&inventory));
        let names = selected
            .iter()
            .map(|interface| interface.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["enp6s0", "wlp5s0"]);
    }

    #[test]
    fn narrow_network_summary_keeps_status_and_compact_rates() {
        let interface = test_interface("enp8s0", OperState::Up);
        let parts = network_summary_parts(&interface, Some("Realtek RTL8125 2.5GbE"), None, 36);
        let text = format!("{}{}{}", parts.0, parts.1, parts.2);

        assert!(text.chars().count() <= 36);
        assert!(text.starts_with("enp8s0"));
        assert!(text.contains("● up"));
        assert!(text.contains("R1.2M/s"));
        assert!(text.contains("T84K/s"));
    }

    fn summary_text(width: usize, temperature: Option<&Temperature>) -> String {
        let interface = test_interface("enp6s0", OperState::Up);
        network_summary_line(
            &interface,
            Some("Realtek RTL8125 2.5GbE"),
            temperature,
            width,
        )
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
    }

    #[test]
    fn nic_temperature_follows_the_traffic_and_outlasts_the_model() {
        let temperature = Temperature {
            key: crate::linux::TemperatureKey::CpuPackage(0),
            celsius: Some(47),
            max: Some(120),
            crit: None,
        };
        let wide = summary_text(100, Some(&temperature));
        assert!(
            wide.starts_with("enp6s0  Realtek RTL8125 2.5GbE  ● up  RX"),
            "{wide}"
        );
        assert!(wide.ends_with("TX 84.2 KiB/s  47°C"), "{wide}");

        for width in 0..100 {
            let text = summary_text(width, Some(&temperature));
            assert!(text.chars().count() <= width.max(4), "{width}: {text}");
            if text.contains("Realtek") {
                assert!(
                    text.contains("47°C"),
                    "the model goes first: {width}: {text}"
                );
            }
            // The temperature never displaces traffic that fits without it.
            let traffic = |text: &str| text.contains("1.2");
            assert_eq!(
                traffic(&text),
                traffic(&summary_text(width, None)),
                "{width}: {text}"
            );
        }
        assert_eq!(summary_text(100, None), {
            let parts = network_summary_parts(
                &test_interface("enp6s0", OperState::Up),
                Some("Realtek RTL8125 2.5GbE"),
                None,
                100,
            );
            format!("{}{}{}", parts.0, parts.1, parts.2)
        });
    }

    #[test]
    fn network_section_limits_visible_interfaces() {
        let mut app = App::default();
        let interfaces = (0..5)
            .map(|index| test_interface(&format!("eth{index}"), OperState::Up))
            .collect();
        app.update(crate::action::Action::NetworkUpdated(
            crate::linux::NetworkSnapshot {
                interfaces,
                error: None,
            },
        ));
        let backend = TestBackend::new(100, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_card(frame, &app, frame.area(), true))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(text.contains("eth0"));
        assert!(text.contains("eth1"));
        assert!(text.contains("eth2"));
        assert!(!text.contains("eth3"));
        assert!(text.contains("2 more interfaces"));
    }

    #[test]
    fn network_summary_hides_cached_rates_until_collection_recovers() {
        let mut app = App::default();
        let healthy = crate::linux::NetworkSnapshot {
            interfaces: vec![test_interface("enp6s0", OperState::Up)],
            error: None,
        };
        app.update(crate::action::Action::NetworkUpdated(healthy.clone()));
        app.update(crate::action::Action::NetworkUpdated(
            crate::linux::NetworkSnapshot {
                interfaces: Vec::new(),
                error: Some("net unavailable".into()),
            },
        ));
        let backend = TestBackend::new(100, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_card(frame, &app, frame.area(), true))
            .unwrap();

        let stale = rendered_text(&terminal);
        assert!(stale.contains("Network data unavailable"));
        assert!(!stale.contains("enp6s0"));
        assert!(!stale.contains("RX"));

        app.update(crate::action::Action::NetworkUpdated(healthy));
        terminal
            .draw(|frame| render_card(frame, &app, frame.area(), true))
            .unwrap();
        let recovered = rendered_text(&terminal);
        assert!(recovered.contains("enp6s0"));
        assert!(recovered.contains("RX"));
        assert!(!recovered.contains("Network data unavailable"));
    }
}
