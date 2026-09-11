use std::{
    collections::HashMap,
    ffi::CStr,
    fs, io,
    net::{Ipv4Addr, Ipv6Addr},
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const PROC_NET_DEV: &str = "/proc/net/dev";
const SYS_CLASS_NET: &str = "/sys/class/net";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OperState {
    Up,
    Down,
    Dormant,
    Testing,
    LowerLayerDown,
    NotPresent,
    #[default]
    Unknown,
}

impl OperState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
            Self::Dormant => "DORMANT",
            Self::Testing => "TESTING",
            Self::LowerLayerDown => "LOWER_DOWN",
            Self::NotPresent => "NOT_PRESENT",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn from_sys(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "up" => Self::Up,
            "down" => Self::Down,
            "dormant" => Self::Dormant,
            "testing" => Self::Testing,
            "lowerlayerdown" => Self::LowerLayerDown,
            "notpresent" => Self::NotPresent,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkInterfaceInfo {
    pub name: String,
    pub operstate: OperState,
    pub mac_address: Option<String>,
    pub mtu: Option<u32>,
    pub ipv4_addresses: Vec<Ipv4Addr>,
    pub ipv6_addresses: Vec<Ipv6Addr>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub rx_rate_bytes_per_sec: Option<f64>,
    pub tx_rate_bytes_per_sec: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetworkSnapshot {
    pub interfaces: Vec<NetworkInterfaceInfo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct InterfacePrev {
    rx_bytes: u64,
    tx_bytes: u64,
    timestamp: Instant,
}

#[derive(Default)]
pub struct NetworkSampler {
    previous: HashMap<String, InterfacePrev>,
}

impl NetworkSampler {
    pub fn collect(&mut self) -> NetworkSnapshot {
        self.collect_at(
            Instant::now(),
            Path::new(PROC_NET_DEV),
            Path::new(SYS_CLASS_NET),
        )
    }

    pub fn collect_at(
        &mut self,
        now: Instant,
        proc_net_dev_path: &Path,
        sys_class_net_path: &Path,
    ) -> NetworkSnapshot {
        let dev_contents = match fs::read_to_string(proc_net_dev_path) {
            Ok(contents) => contents,
            Err(error) => {
                return NetworkSnapshot {
                    interfaces: Vec::new(),
                    error: Some(format!(
                        "Failed to read {}: {error}",
                        proc_net_dev_path.display()
                    )),
                };
            }
        };

        let raw_stats = parse_proc_net_dev(&dev_contents);
        let (mut ipv4_map, mut ipv6_map) = collect_ip_addresses();

        let mut discovered_names: Vec<String> = raw_stats.iter().map(|s| s.name.clone()).collect();

        // Also check if sys_class_net has interfaces not present in proc_net_dev
        if let Ok(entries) = fs::read_dir(sys_class_net_path) {
            for entry in entries.flatten() {
                if let Ok(file_name) = entry.file_name().into_string() {
                    if !discovered_names.contains(&file_name) {
                        discovered_names.push(file_name);
                    }
                }
            }
        }

        // Sort: non-loopback first alphabetically, loopback at the end
        discovered_names.sort_by(|a, b| {
            let a_lo = a == "lo";
            let b_lo = b == "lo";
            (a_lo, a).cmp(&(b_lo, b))
        });

        let mut next_previous = HashMap::with_capacity(discovered_names.len());
        let mut interfaces = Vec::with_capacity(discovered_names.len());

        for name in discovered_names {
            let stats = raw_stats.iter().find(|s| s.name == name);
            let rx_bytes = stats.map_or(0, |s| s.rx_bytes);
            let tx_bytes = stats.map_or(0, |s| s.tx_bytes);
            let rx_packets = stats.map_or(0, |s| s.rx_packets);
            let tx_packets = stats.map_or(0, |s| s.tx_packets);
            let rx_errors = stats.map_or(0, |s| s.rx_errors);
            let tx_errors = stats.map_or(0, |s| s.tx_errors);
            let rx_dropped = stats.map_or(0, |s| s.rx_dropped);
            let tx_dropped = stats.map_or(0, |s| s.tx_dropped);

            let iface_sys_dir = sys_class_net_path.join(&name);
            let operstate = read_operstate(&iface_sys_dir);
            let mac_address = read_mac_address(&iface_sys_dir);
            let mtu = read_mtu(&iface_sys_dir);

            let ipv4 = ipv4_map.remove(&name).unwrap_or_default();
            let ipv6 = ipv6_map.remove(&name).unwrap_or_default();

            let (rx_rate, tx_rate) = if let Some(prev) = self.previous.get(&name) {
                let elapsed = now.saturating_duration_since(prev.timestamp).as_secs_f64();
                if elapsed >= 0.001 {
                    let rx_rate = if rx_bytes >= prev.rx_bytes {
                        Some((rx_bytes - prev.rx_bytes) as f64 / elapsed)
                    } else {
                        // Counter reset/wrap
                        Some(0.0)
                    };
                    let tx_rate = if tx_bytes >= prev.tx_bytes {
                        Some((tx_bytes - prev.tx_bytes) as f64 / elapsed)
                    } else {
                        Some(0.0)
                    };
                    (rx_rate, tx_rate)
                } else {
                    (None, None)
                }
            } else {
                // First sample or newly appeared interface
                (None, None)
            };

            next_previous.insert(
                name.clone(),
                InterfacePrev {
                    rx_bytes,
                    tx_bytes,
                    timestamp: now,
                },
            );

            interfaces.push(NetworkInterfaceInfo {
                name,
                operstate,
                mac_address,
                mtu,
                ipv4_addresses: ipv4,
                ipv6_addresses: ipv6,
                rx_bytes,
                tx_bytes,
                rx_packets,
                tx_packets,
                rx_errors,
                tx_errors,
                rx_dropped,
                tx_dropped,
                rx_rate_bytes_per_sec: rx_rate,
                tx_rate_bytes_per_sec: tx_rate,
            });
        }

        self.previous = next_previous;

        NetworkSnapshot {
            interfaces,
            error: None,
        }
    }
}

pub struct NetworkCollector {
    receiver: Receiver<NetworkSnapshot>,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl NetworkCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (snapshot_tx, receiver) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("network-metrics".into())
            .spawn(move || {
                let mut sampler = NetworkSampler::default();

                loop {
                    if snapshot_tx.send(sampler.collect()).is_err() {
                        break;
                    }

                    match stop_rx.recv_timeout(refresh_rate) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;

        Ok(Self {
            receiver,
            stop,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<NetworkSnapshot> {
        self.receiver.try_iter().last()
    }
}

impl Drop for NetworkCollector {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawNetDevRow {
    pub name: String,
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_dropped: u64,
}

pub fn parse_proc_net_dev(contents: &str) -> Vec<RawNetDevRow> {
    let mut rows = Vec::new();

    for line in contents.lines() {
        let Some((name_part, stats_part)) = line.split_once(':') else {
            continue;
        };

        let name = name_part.trim().to_string();
        if name.is_empty() {
            continue;
        }

        let fields: Vec<u64> = stats_part
            .split_whitespace()
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();

        if fields.len() >= 16 {
            rows.push(RawNetDevRow {
                name,
                rx_bytes: fields[0],
                rx_packets: fields[1],
                rx_errors: fields[2],
                rx_dropped: fields[3],
                tx_bytes: fields[8],
                tx_packets: fields[9],
                tx_errors: fields[10],
                tx_dropped: fields[11],
            });
        }
    }

    rows
}

fn read_operstate(iface_dir: &Path) -> OperState {
    fs::read_to_string(iface_dir.join("operstate"))
        .ok()
        .map_or(OperState::Unknown, |s| OperState::from_sys(&s))
}

fn read_mac_address(iface_dir: &Path) -> Option<String> {
    fs::read_to_string(iface_dir.join("address"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "00:00:00:00:00:00")
}

fn read_mtu(iface_dir: &Path) -> Option<u32> {
    fs::read_to_string(iface_dir.join("mtu"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

fn collect_ip_addresses() -> (
    HashMap<String, Vec<Ipv4Addr>>,
    HashMap<String, Vec<Ipv6Addr>>,
) {
    let mut ipv4_map: HashMap<String, Vec<Ipv4Addr>> = HashMap::new();
    let mut ipv6_map: HashMap<String, Vec<Ipv6Addr>> = HashMap::new();

    let mut ifaddrs_ptr: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifaddrs_ptr) } != 0 || ifaddrs_ptr.is_null() {
        return (ipv4_map, ipv6_map);
    }

    let mut cursor = ifaddrs_ptr;
    while !cursor.is_null() {
        let ifa = unsafe { &*cursor };
        if !ifa.ifa_name.is_null() && !ifa.ifa_addr.is_null() {
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let family = unsafe { (*ifa.ifa_addr).sa_family as libc::c_int };
            if family == libc::AF_INET {
                let sin = unsafe { *(ifa.ifa_addr as *const libc::sockaddr_in) };
                let ip = Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes());
                let list = ipv4_map.entry(name).or_default();
                if !list.contains(&ip) {
                    list.push(ip);
                }
            } else if family == libc::AF_INET6 {
                let sin6 = unsafe { *(ifa.ifa_addr as *const libc::sockaddr_in6) };
                let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                let list = ipv6_map.entry(name).or_default();
                if !list.contains(&ip) {
                    list.push(ip);
                }
            }
        }
        cursor = ifa.ifa_next;
    }

    unsafe { libc::freeifaddrs(ifaddrs_ptr) };
    (ipv4_map, ipv6_map)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SAMPLE_PROC_NET_DEV: &str = r#"Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 447897079 7585865    0    0    0     0          0         0 447897079 7585865    0    0    0     0       0          0
enp6s0: 15409598376 10794652    0    0    0     0          0     10235 606119989 5658836    5   15    0     0       0          0
docker0:   16306     214    0    0    0     0          0         0   593023    1043    0    1    0     0       0          0
"#;

    #[test]
    fn parses_proc_net_dev_correctly() {
        let rows = parse_proc_net_dev(SAMPLE_PROC_NET_DEV);
        assert_eq!(rows.len(), 3);

        assert_eq!(rows[0].name, "lo");
        assert_eq!(rows[0].rx_bytes, 447897079);
        assert_eq!(rows[0].rx_packets, 7585865);
        assert_eq!(rows[0].tx_bytes, 447897079);
        assert_eq!(rows[0].tx_packets, 7585865);

        assert_eq!(rows[1].name, "enp6s0");
        assert_eq!(rows[1].rx_bytes, 15409598376);
        assert_eq!(rows[1].rx_packets, 10794652);
        assert_eq!(rows[1].tx_bytes, 606119989);
        assert_eq!(rows[1].tx_packets, 5658836);
        assert_eq!(rows[1].tx_errors, 5);
        assert_eq!(rows[1].tx_dropped, 15);

        assert_eq!(rows[2].name, "docker0");
        assert_eq!(rows[2].rx_bytes, 16306);
        assert_eq!(rows[2].tx_bytes, 593023);
    }

    #[test]
    fn parses_operstate_strings() {
        assert_eq!(OperState::from_sys("up\n"), OperState::Up);
        assert_eq!(OperState::from_sys("DOWN"), OperState::Down);
        assert_eq!(OperState::from_sys("dormant"), OperState::Dormant);
        assert_eq!(OperState::from_sys("testing"), OperState::Testing);
        assert_eq!(
            OperState::from_sys("lowerlayerdown"),
            OperState::LowerLayerDown
        );
        assert_eq!(OperState::from_sys("notpresent"), OperState::NotPresent);
        assert_eq!(OperState::from_sys("unknown"), OperState::Unknown);
        assert_eq!(OperState::from_sys("custom"), OperState::Unknown);
    }

    #[test]
    fn calculates_transfer_rates_between_samples() {
        let mut sampler = NetworkSampler::default();
        let t0 = Instant::now();

        // Simulate first sample: populate previous map
        sampler.previous.insert(
            "enp6s0".into(),
            InterfacePrev {
                rx_bytes: 1_000_000,
                tx_bytes: 500_000,
                timestamp: t0,
            },
        );

        let t1 = t0 + Duration::from_secs(2);
        // After 2 seconds, 2 MiB received (+2_097_152 bytes) and 1 MiB sent (+1_048_576 bytes)
        let rx_now = 1_000_000 + 2_097_152;
        let tx_now = 500_000 + 1_048_576;

        let elapsed = t1.saturating_duration_since(t0).as_secs_f64();
        let rx_rate = (rx_now - 1_000_000) as f64 / elapsed;
        let tx_rate = (tx_now - 500_000) as f64 / elapsed;

        assert!((rx_rate - 1_048_576.0).abs() < 1.0);
        assert!((tx_rate - 524_288.0).abs() < 1.0);
    }

    #[test]
    fn handles_first_sample_without_treating_total_as_rate() {
        let sampler = NetworkSampler::default();
        assert!(sampler.previous.is_empty());
        // For a new interface, previous sample is None so rate should be None
        assert_eq!(sampler.previous.get("eth0").copied().map(|_| ()), None);
    }

    #[test]
    fn handles_counter_reset_gracefully() {
        let mut sampler = NetworkSampler::default();
        let t0 = Instant::now();

        sampler.previous.insert(
            "enp6s0".into(),
            InterfacePrev {
                rx_bytes: 10_000_000,
                tx_bytes: 5_000_000,
                timestamp: t0,
            },
        );

        // Current counter is smaller than previous (reboot, reset, or wrap)
        let rx_now = 100;
        let tx_now = 50;

        let prev = sampler.previous.get("enp6s0").unwrap();
        let rx_rate = if rx_now >= prev.rx_bytes {
            Some((rx_now - prev.rx_bytes) as f64 / 1.0)
        } else {
            Some(0.0)
        };
        let tx_rate = if tx_now >= prev.tx_bytes {
            Some((tx_now - prev.tx_bytes) as f64 / 1.0)
        } else {
            Some(0.0)
        };

        assert_eq!(rx_rate, Some(0.0));
        assert_eq!(tx_rate, Some(0.0));
    }

    #[test]
    fn handles_interface_appearance_and_disappearance() {
        let mut sampler = NetworkSampler::default();
        let t0 = Instant::now();

        sampler.previous.insert(
            "veth123".into(),
            InterfacePrev {
                rx_bytes: 1000,
                tx_bytes: 1000,
                timestamp: t0,
            },
        );

        // Next snapshot only has enp6s0; veth123 disappeared
        let mut next_previous = HashMap::new();
        next_previous.insert(
            "enp6s0".into(),
            InterfacePrev {
                rx_bytes: 2000,
                tx_bytes: 2000,
                timestamp: t0 + Duration::from_secs(1),
            },
        );
        sampler.previous = next_previous;

        assert!(!sampler.previous.contains_key("veth123"));
        assert!(sampler.previous.contains_key("enp6s0"));
    }
}
