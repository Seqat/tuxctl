use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::App,
    linux::{HardwareInventory, NetworkInterfaceInfo, OperState},
};

use super::{hardware::section_heading, layout, network};

const MAX_NETWORK_INTERFACES: usize = 3;

pub(super) fn desired_height(app: &App, inventory: Option<&HardwareInventory>) -> u16 {
    let network_count = overview_network_interfaces(app.networks(), inventory).len();
    1_u16
        .saturating_add(u16::try_from(network_count.min(MAX_NETWORK_INTERFACES)).unwrap_or(3))
        .saturating_add(u16::from(network_count > MAX_NETWORK_INTERFACES))
}

pub(super) fn render(
    frame: &mut Frame,
    app: &App,
    inventory: Option<&HardwareInventory>,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![section_heading("NETWORK")];
    let remaining = usize::from(area.height).saturating_sub(1);
    if remaining == 0 {
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    if app.networks().is_empty() {
        lines.push(Line::from(if app.network_error().is_some() {
            "Network data unavailable"
        } else {
            "No interfaces found"
        }));
    } else {
        let interfaces = overview_network_interfaces(app.networks(), inventory);
        if interfaces.is_empty() {
            lines.push(Line::from("No hardware interfaces found"));
            frame.render_widget(Paragraph::new(lines), area);
            return;
        }
        let show_overflow = interfaces.len() > MAX_NETWORK_INTERFACES && remaining > 1;
        let interface_limit = interfaces
            .len()
            .min(MAX_NETWORK_INTERFACES)
            .min(remaining.saturating_sub(usize::from(show_overflow)));
        lines.extend(interfaces.iter().take(interface_limit).map(|interface| {
            let model = inventory.and_then(|inventory| {
                inventory
                    .network_devices
                    .iter()
                    .find(|device| device.interface_name == interface.name)
                    .and_then(|device| device.model.as_deref())
            });
            network_summary_line(interface, model, width)
        }));
        if show_overflow {
            lines.push(Line::from(format!(
                "… {} more interfaces",
                interfaces.len() - interface_limit
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn overview_network_interfaces<'a>(
    interfaces: &'a [NetworkInterfaceInfo],
    inventory: Option<&HardwareInventory>,
) -> Vec<&'a NetworkInterfaceInfo> {
    let is_physical = |name: &str| {
        inventory.is_some_and(|inventory| {
            inventory
                .network_devices
                .iter()
                .any(|device| device.interface_name == name)
        })
    };
    let mut relevant = interfaces
        .iter()
        .filter(|interface| is_physical(&interface.name) || !is_noisy_network_name(&interface.name))
        .collect::<Vec<_>>();
    relevant.sort_by_key(|interface| !is_physical(&interface.name));
    relevant
}

fn is_noisy_network_name(name: &str) -> bool {
    name == "lo"
        || [
            "docker",
            "veth",
            "br-",
            "virbr",
            "cni",
            "flannel",
            "cali",
            "kube",
            "vboxnet",
            "vmnet",
            "tun",
            "tap",
            "wg",
            "tailscale",
            "sit",
            "ip6tnl",
            "gre",
            "gretap",
            "erspan",
            "geneve",
            "vxlan",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn network_summary_line(
    interface: &NetworkInterfaceInfo,
    model: Option<&str>,
    width: usize,
) -> Line<'static> {
    let (prefix, state, suffix) = network_summary_parts(interface, model, width);
    let (_, state_style) = network::state_display(interface.operstate);
    Line::from(vec![
        Span::raw(prefix),
        Span::styled(state, state_style),
        Span::raw(suffix),
    ])
}

fn network_summary_parts(
    interface: &NetworkInterfaceInfo,
    model: Option<&str>,
    width: usize,
) -> (String, String, String) {
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

    if let Some(model) = model {
        let occupied = prefix.chars().count() + state.chars().count() + suffix.chars().count();
        let model_width = width.saturating_sub(occupied + 2);
        if model_width >= 4 {
            prefix.push_str(&layout::truncate(model, model_width));
            prefix.push_str("  ");
        }
    }

    (prefix, state, suffix)
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

fn format_rate_tight(rate: Option<f64>) -> String {
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

    #[test]
    fn network_summary_handles_models_rates_and_down_interfaces() {
        let up = test_interface("enp8s0", OperState::Up);
        let with_model = network_summary_parts(&up, Some("Realtek RTL8125 2.5GbE"), 100);
        let with_model = format!("{}{}{}", with_model.0, with_model.1, with_model.2);
        assert!(with_model.contains("enp8s0  Realtek RTL8125 2.5GbE  ● up"));
        assert!(with_model.contains("RX 1.2 MiB/s"));
        assert!(with_model.contains("TX 84.2 KiB/s"));

        let without_model = network_summary_parts(&up, None, 100);
        let without_model = format!("{}{}{}", without_model.0, without_model.1, without_model.2);
        assert!(without_model.starts_with("enp8s0  ● up"));

        let mut down = test_interface("wlp5s0", OperState::Down);
        down.rx_rate_bytes_per_sec = Some(0.0);
        down.tx_rate_bytes_per_sec = None;
        let down = network_summary_parts(&down, Some("Intel Wi-Fi 6E AX210"), 80);
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
            }],
            ..HardwareInventory::default()
        };

        let selected = overview_network_interfaces(&interfaces, Some(&inventory));
        let names = selected
            .iter()
            .map(|interface| interface.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["enp6s0", "wlp5s0"]);
    }

    #[test]
    fn narrow_network_summary_keeps_status_and_compact_rates() {
        let interface = test_interface("enp8s0", OperState::Up);
        let parts = network_summary_parts(&interface, Some("Realtek RTL8125 2.5GbE"), 36);
        let text = format!("{}{}{}", parts.0, parts.1, parts.2);

        assert!(text.chars().count() <= 36);
        assert!(text.starts_with("enp8s0"));
        assert!(text.contains("● up"));
        assert!(text.contains("R1.2M/s"));
        assert!(text.contains("T84K/s"));
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
            .draw(|frame| crate::ui::hardware::render(frame, &app, frame.area()))
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
}
