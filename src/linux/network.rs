use std::{
    collections::HashMap,
    ffi::CStr,
    fs, io,
    net::{Ipv4Addr, Ipv6Addr},
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    control::{run_periodic, CollectorControl},
    latest_snapshot::{self, LatestReceiver},
    rate::CounterSample,
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

/// RX/TX byte counters of one interface at one sample.
type InterfacePrev = CounterSample<2>;

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
                self.previous.clear();
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

            let (rx_rate, tx_rate) = if stats.is_some() {
                let sample = InterfacePrev::new([rx_bytes, tx_bytes], now);
                let [rx_rate, tx_rate] = sample.rates_since(self.previous.get(&name));
                next_previous.insert(name.clone(), sample);
                (rx_rate, tx_rate)
            } else {
                (None, None)
            };

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
    receiver: LatestReceiver<NetworkSnapshot>,
    control: Arc<CollectorControl>,
    worker: Option<JoinHandle<()>>,
}

impl NetworkCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (snapshot_tx, receiver) = latest_snapshot::channel();
        let control = Arc::new(CollectorControl::new(refresh_rate, false));
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("network-metrics".into())
            .spawn(move || {
                let mut sampler = NetworkSampler::default();

                run_periodic(
                    &worker_control,
                    || sampler.collect(),
                    |snapshot| snapshot_tx.publish(snapshot),
                );
            })?;

        Ok(Self {
            receiver,
            control,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<NetworkSnapshot> {
        self.receiver.take_latest()
    }

    /// Applies a new sampling period to the running worker.
    pub fn set_period(&self, period: Duration) {
        self.control.set_period(period);
    }
}

impl Drop for NetworkCollector {
    fn drop(&mut self) {
        self.control.stop();
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
    fn sampling_replaces_disappeared_interfaces_and_tracks_new_ones() {
        let root = std::env::temp_dir().join(format!(
            "tuxctl-network-lifecycle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let proc_net_dev = root.join("net-dev");
        let sys_class_net = root.join("net");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&sys_class_net).unwrap();

        let mut sampler = NetworkSampler::default();
        let t0 = Instant::now();
        fs::write(&proc_net_dev, net_dev_row("tuxgone0", 1_000, 2_000)).unwrap();
        let first = sampler.collect_at(t0, &proc_net_dev, &sys_class_net);

        assert_eq!(first.interfaces.len(), 1);
        assert_eq!(first.interfaces[0].name, "tuxgone0");
        assert_eq!(first.interfaces[0].rx_rate_bytes_per_sec, None);
        assert_eq!(first.interfaces[0].tx_rate_bytes_per_sec, None);

        fs::write(&proc_net_dev, net_dev_row("tuxnew0", 3_000, 4_000)).unwrap();
        let second = sampler.collect_at(t0 + Duration::from_secs(1), &proc_net_dev, &sys_class_net);

        assert_eq!(second.interfaces.len(), 1);
        assert_eq!(second.interfaces[0].name, "tuxnew0");
        assert_eq!(second.interfaces[0].rx_rate_bytes_per_sec, None);
        assert!(!sampler.previous.contains_key("tuxgone0"));
        assert!(sampler.previous.contains_key("tuxnew0"));

        fs::write(&proc_net_dev, net_dev_row("tuxnew0", 3_500, 5_000)).unwrap();
        let third = sampler.collect_at(t0 + Duration::from_secs(3), &proc_net_dev, &sys_class_net);

        assert_eq!(third.interfaces[0].rx_rate_bytes_per_sec, Some(250.0));
        assert_eq!(third.interfaces[0].tx_rate_bytes_per_sec, Some(500.0));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proc_read_failure_invalidates_rate_baselines_before_recovery() {
        let root = std::env::temp_dir().join(format!(
            "tuxctl-network-read-failure-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let proc_net_dev = root.join("net-dev");
        let sys_class_net = root.join("net");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&sys_class_net).unwrap();

        let mut sampler = NetworkSampler::default();
        let t0 = Instant::now();
        fs::write(&proc_net_dev, net_dev_row("enp6s0", 1_000, 2_000)).unwrap();
        let baseline = sampler.collect_at(t0, &proc_net_dev, &sys_class_net);

        assert_eq!(baseline.interfaces.len(), 1);
        assert_eq!(baseline.interfaces[0].rx_rate_bytes_per_sec, None);
        assert_eq!(baseline.interfaces[0].tx_rate_bytes_per_sec, None);
        assert!(sampler.previous.contains_key("enp6s0"));

        fs::remove_file(&proc_net_dev).unwrap();
        let failed = sampler.collect_at(t0 + Duration::from_secs(1), &proc_net_dev, &sys_class_net);

        assert!(failed.interfaces.is_empty());
        assert!(failed.error.as_deref().is_some_and(
            |error| error.starts_with(&format!("Failed to read {}:", proc_net_dev.display()))
        ));
        assert!(sampler.previous.is_empty());

        fs::write(
            &proc_net_dev,
            net_dev_row("enp6s0", 8 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024),
        )
        .unwrap();
        let recovered =
            sampler.collect_at(t0 + Duration::from_secs(2), &proc_net_dev, &sys_class_net);

        assert_eq!(recovered.interfaces.len(), 1);
        assert_eq!(recovered.interfaces[0].name, "enp6s0");
        assert_eq!(recovered.interfaces[0].rx_rate_bytes_per_sec, None);
        assert_eq!(recovered.interfaces[0].tx_rate_bytes_per_sec, None);
        assert!(sampler.previous.contains_key("enp6s0"));

        fs::write(
            &proc_net_dev,
            net_dev_row(
                "enp6s0",
                8 * 1024 * 1024 * 1024 + 4_096,
                2 * 1024 * 1024 * 1024 + 2_048,
            ),
        )
        .unwrap();
        let resumed =
            sampler.collect_at(t0 + Duration::from_secs(3), &proc_net_dev, &sys_class_net);

        assert_eq!(resumed.interfaces[0].rx_rate_bytes_per_sec, Some(4_096.0));
        assert_eq!(resumed.interfaces[0].tx_rate_bytes_per_sec, Some(2_048.0));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sysfs_only_discovery_never_becomes_a_counter_baseline() {
        let root = std::env::temp_dir().join(format!(
            "tuxctl-network-sysfs-baseline-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let proc_net_dev = root.join("net-dev");
        let sys_class_net = root.join("net");
        let interface_dir = sys_class_net.join("tuxsys0");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&interface_dir).unwrap();

        let mut sampler = NetworkSampler::default();
        let t0 = Instant::now();
        fs::write(&proc_net_dev, "").unwrap();
        let sysfs_only = sampler.collect_at(t0, &proc_net_dev, &sys_class_net);

        assert_eq!(sysfs_only.interfaces.len(), 1);
        assert_eq!(sysfs_only.interfaces[0].name, "tuxsys0");
        assert_eq!(sysfs_only.interfaces[0].rx_rate_bytes_per_sec, None);
        assert_eq!(sysfs_only.interfaces[0].tx_rate_bytes_per_sec, None);
        assert!(!sampler.previous.contains_key("tuxsys0"));

        fs::write(
            &proc_net_dev,
            net_dev_row("tuxsys0", 8 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024),
        )
        .unwrap();
        let first_counters =
            sampler.collect_at(t0 + Duration::from_secs(1), &proc_net_dev, &sys_class_net);
        assert_eq!(first_counters.interfaces[0].rx_rate_bytes_per_sec, None);
        assert_eq!(first_counters.interfaces[0].tx_rate_bytes_per_sec, None);
        assert!(sampler.previous.contains_key("tuxsys0"));

        fs::write(
            &proc_net_dev,
            net_dev_row(
                "tuxsys0",
                8 * 1024 * 1024 * 1024 + 4_096,
                2 * 1024 * 1024 * 1024 + 2_048,
            ),
        )
        .unwrap();
        let next_counters =
            sampler.collect_at(t0 + Duration::from_secs(2), &proc_net_dev, &sys_class_net);
        assert_eq!(
            next_counters.interfaces[0].rx_rate_bytes_per_sec,
            Some(4_096.0)
        );
        assert_eq!(
            next_counters.interfaces[0].tx_rate_bytes_per_sec,
            Some(2_048.0)
        );

        fs::write(&proc_net_dev, "").unwrap();
        let counters_lost =
            sampler.collect_at(t0 + Duration::from_secs(3), &proc_net_dev, &sys_class_net);
        assert_eq!(counters_lost.interfaces[0].rx_rate_bytes_per_sec, None);
        assert!(!sampler.previous.contains_key("tuxsys0"));

        fs::write(
            &proc_net_dev,
            net_dev_row("tuxsys0", 9 * 1024 * 1024 * 1024, 3 * 1024 * 1024 * 1024),
        )
        .unwrap();
        let counters_return =
            sampler.collect_at(t0 + Duration::from_secs(4), &proc_net_dev, &sys_class_net);
        assert_eq!(counters_return.interfaces[0].rx_rate_bytes_per_sec, None);
        assert_eq!(counters_return.interfaces[0].tx_rate_bytes_per_sec, None);

        fs::write(
            &proc_net_dev,
            net_dev_row(
                "tuxsys0",
                9 * 1024 * 1024 * 1024 + 1_024,
                3 * 1024 * 1024 * 1024 + 512,
            ),
        )
        .unwrap();
        let counters_resume =
            sampler.collect_at(t0 + Duration::from_secs(5), &proc_net_dev, &sys_class_net);
        assert_eq!(
            counters_resume.interfaces[0].rx_rate_bytes_per_sec,
            Some(1_024.0)
        );
        assert_eq!(
            counters_resume.interfaces[0].tx_rate_bytes_per_sec,
            Some(512.0)
        );

        let _ = fs::remove_dir_all(root);
    }

    fn net_dev_row(name: &str, rx_bytes: u64, tx_bytes: u64) -> String {
        format!("{name}: {rx_bytes} 1 0 0 0 0 0 0 {tx_bytes} 1 0 0 0 0 0 0\n")
    }
}
