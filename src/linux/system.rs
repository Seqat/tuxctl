use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    io::{self, Read},
    mem::MaybeUninit,
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    control::{run_periodic, CollectorControl},
    hardware::is_whole_disk,
    latest_snapshot::{self, LatestReceiver},
    rate::CounterSample,
};

const PROC_STAT: &str = "/proc/stat";
const PROC_MEMINFO: &str = "/proc/meminfo";
const PROC_UPTIME: &str = "/proc/uptime";
const PROC_LOADAVG: &str = "/proc/loadavg";
const PROC_DISKSTATS: &str = "/proc/diskstats";
/// /proc/diskstats counts sectors in 512-byte units, whatever the device's
/// real sector size.
const DISKSTATS_SECTOR_BYTES: f64 = 512.0;
const PROC_HOSTNAME: &str = "/proc/sys/kernel/hostname";
const PROC_KERNEL_RELEASE: &str = "/proc/sys/kernel/osrelease";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemMetrics {
    pub cpu_percent: Option<f64>,
    pub logical_cpus: Vec<LogicalCpuMetrics>,
    pub memory: Option<ByteUsage>,
    pub uptime: Option<Duration>,
    pub load_average: Option<LoadAverage>,
    pub root_filesystem: Option<ByteUsage>,
    pub system_identity: SystemIdentity,
    /// Throughput of whole disks; empty when /proc/diskstats is unreadable.
    pub disks: Vec<DiskIo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskIo {
    /// Kernel name, matching `StorageDevice::system_name`.
    pub name: Arc<str>,
    /// `None` until a second sample exists for this disk.
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalCpuMetrics {
    pub id: LogicalCpuId,
    pub utilization_percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogicalCpuId(u32);

impl LogicalCpuId {
    pub const fn index(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn for_test(index: u32) -> Self {
        Self(index)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemIdentity {
    pub hostname: Option<String>,
    pub kernel_release: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteUsage {
    pub used: u64,
    pub total: u64,
}

impl ByteUsage {
    pub fn percent(self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }

        (self.used.min(self.total) as f64 / self.total as f64) * 100.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

pub struct SystemMetricsCollector {
    receiver: LatestReceiver<SystemMetrics>,
    control: Arc<CollectorControl>,
    worker: Option<JoinHandle<()>>,
}

impl SystemMetricsCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (metrics_tx, receiver) = latest_snapshot::channel();
        let control = Arc::new(CollectorControl::new(refresh_rate, false));
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("system-metrics".into())
            .spawn(move || {
                let identity = collect_system_identity();
                let mut sampler = SystemMetricsSampler::new(identity);

                run_periodic(
                    &worker_control,
                    || sampler.collect(),
                    |snapshot| metrics_tx.publish(snapshot),
                );
            })?;

        Ok(Self {
            receiver,
            control,
            worker: Some(worker),
        })
    }

    pub fn latest(&self) -> Option<SystemMetrics> {
        self.receiver.take_latest()
    }

    /// Applies a new sampling period to the running worker.
    pub fn set_period(&self, period: Duration) {
        self.control.set_period(period);
    }
}

impl Drop for SystemMetricsCollector {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CpuSample {
    aggregate: CpuTimes,
    logical: BTreeMap<LogicalCpuId, CpuTimes>,
}

struct SystemMetricsSampler {
    previous_cpu: Option<CpuSample>,
    system_identity: SystemIdentity,
    disks: DiskIoSampler,
}

impl SystemMetricsSampler {
    fn new(system_identity: SystemIdentity) -> Self {
        Self {
            previous_cpu: None,
            system_identity,
            disks: DiskIoSampler::default(),
        }
    }

    fn collect(&mut self) -> SystemMetrics {
        let current_cpu = fs::read_to_string(PROC_STAT)
            .ok()
            .and_then(|contents| parse_cpu_sample(&contents));
        let (cpu_percent, logical_cpus) = current_cpu.as_ref().map_or_else(
            || (None, Vec::new()),
            |current| cpu_metrics(self.previous_cpu.as_ref(), current),
        );
        if current_cpu.is_some() {
            self.previous_cpu = current_cpu;
        }

        SystemMetrics {
            cpu_percent,
            logical_cpus,
            memory: fs::read_to_string(PROC_MEMINFO)
                .ok()
                .and_then(|contents| parse_memory_usage(&contents)),
            uptime: fs::read_to_string(PROC_UPTIME)
                .ok()
                .and_then(|contents| parse_uptime(&contents)),
            load_average: fs::read_to_string(PROC_LOADAVG)
                .ok()
                .and_then(|contents| parse_load_average(&contents)),
            root_filesystem: filesystem_usage("/").ok(),
            system_identity: self.system_identity.clone(),
            disks: self
                .disks
                .collect(Path::new(PROC_DISKSTATS), Instant::now()),
        }
    }
}

/// Read/write rates of whole disks from one read of /proc/diskstats per sample.
#[derive(Default)]
struct DiskIoSampler {
    /// Reused between samples so steady-state reads do not allocate.
    buffer: String,
    /// Sector counters of the last sample; one entry per disk present then.
    previous: Vec<(Arc<str>, CounterSample<2>)>,
}

impl DiskIoSampler {
    fn collect(&mut self, path: &Path, now: Instant) -> Vec<DiskIo> {
        self.buffer.clear();
        let read = fs::File::open(path).and_then(|mut file| file.read_to_string(&mut self.buffer));
        if read.is_err() {
            // A later successful read must not compute rates across the gap.
            self.previous.clear();
            return Vec::new();
        }

        let mut next = Vec::with_capacity(self.previous.len());
        let mut disks = Vec::with_capacity(self.previous.len());
        for (name, sectors) in parse_diskstats(&self.buffer) {
            if next.iter().any(|(seen, _): &(Arc<str>, _)| **seen == *name) {
                continue;
            }
            let previous = self.previous.iter().find(|(known, _)| **known == *name);
            let name = previous.map_or_else(|| Arc::from(name), |(known, _)| Arc::clone(known));
            let sample = CounterSample::new(sectors, now);
            let [read, write] = sample.rates_since(previous.map(|(_, sample)| sample));
            next.push((Arc::clone(&name), sample));
            disks.push(DiskIo {
                name,
                read_bytes_per_sec: read.map(|sectors| sectors * DISKSTATS_SECTOR_BYTES),
                write_bytes_per_sec: write.map(|sectors| sectors * DISKSTATS_SECTOR_BYTES),
            });
        }
        // Disks that disappeared drop out here, so a reappearing disk starts
        // without a rate instead of spanning the gap.
        self.previous = next;
        disks
    }
}

/// Whole-disk `(name, [sectors read, sectors written])` rows. Lines with fewer
/// than the 14 fields of the oldest format, or non-numeric counters, are skipped.
fn parse_diskstats(contents: &str) -> impl Iterator<Item = (&str, [u64; 2])> {
    contents.lines().filter_map(|line| {
        // major minor name, then reads, reads merged, sectors read, ms reading,
        // writes, writes merged, sectors written, ms writing, in flight, ms, weighted ms.
        let mut fields = line.split_whitespace().skip(2);
        let name = fields.next()?;
        if !is_whole_disk(name) {
            return None;
        }
        let mut counters = [0_u64; 11];
        for counter in &mut counters {
            *counter = fields.next()?.parse().ok()?;
        }
        Some((name, [counters[2], counters[6]]))
    })
}

fn parse_cpu_sample(contents: &str) -> Option<CpuSample> {
    let mut aggregate = None;
    let mut logical = BTreeMap::new();

    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(label) = fields.next() else {
            continue;
        };
        if label == "cpu" {
            aggregate = parse_cpu_times(fields);
        } else if let Some(index) = label
            .strip_prefix("cpu")
            .and_then(|index| index.parse::<u32>().ok())
        {
            if let Some(times) = parse_cpu_times(fields) {
                logical.insert(LogicalCpuId(index), times);
            }
        }
    }

    Some(CpuSample {
        aggregate: aggregate?,
        logical,
    })
}

fn parse_cpu_times<'a>(fields: impl Iterator<Item = &'a str>) -> Option<CpuTimes> {
    let values = fields
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }

    let total = values
        .iter()
        .try_fold(0_u64, |total, value| total.checked_add(*value))?;
    let idle = values[3].checked_add(values.get(4).copied().unwrap_or(0))?;

    Some(CpuTimes { idle, total })
}

fn cpu_metrics(
    previous: Option<&CpuSample>,
    current: &CpuSample,
) -> (Option<f64>, Vec<LogicalCpuMetrics>) {
    let aggregate =
        previous.and_then(|previous| cpu_utilization(previous.aggregate, current.aggregate));
    let logical = current
        .logical
        .iter()
        .map(|(&id, &times)| LogicalCpuMetrics {
            id,
            utilization_percent: previous
                .and_then(|previous| previous.logical.get(&id))
                .and_then(|&previous| cpu_utilization(previous, times)),
        })
        .collect();

    (aggregate, logical)
}

fn cpu_utilization(previous: CpuTimes, current: CpuTimes) -> Option<f64> {
    let total_delta = current.total.checked_sub(previous.total)?;
    let idle_delta = current.idle.checked_sub(previous.idle)?;
    if total_delta == 0 || idle_delta > total_delta {
        return None;
    }

    Some(((total_delta - idle_delta) as f64 / total_delta as f64) * 100.0)
}

fn parse_memory_usage(contents: &str) -> Option<ByteUsage> {
    let value = |name: &str| -> Option<u64> {
        contents.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key == name)
                .then(|| value.split_whitespace().next()?.parse::<u64>().ok())
                .flatten()
        })
    };

    let total_kib = value("MemTotal")?;
    let available_kib = value("MemAvailable").or_else(|| {
        value("MemFree")?
            .checked_add(value("Buffers")?)?
            .checked_add(value("Cached")?)
    })?;
    let total = total_kib.checked_mul(1024)?;
    let available = available_kib.checked_mul(1024)?;

    Some(ByteUsage {
        used: total.saturating_sub(available),
        total,
    })
}

fn parse_uptime(contents: &str) -> Option<Duration> {
    let seconds = contents.split_whitespace().next()?.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds.is_sign_negative() {
        return None;
    }

    Some(Duration::from_secs_f64(seconds))
}

fn parse_load_average(contents: &str) -> Option<LoadAverage> {
    let mut values = contents.split_whitespace().take(3).map(str::parse::<f64>);
    Some(LoadAverage {
        one: values.next()?.ok()?,
        five: values.next()?.ok()?,
        fifteen: values.next()?.ok()?,
    })
}

fn collect_system_identity() -> SystemIdentity {
    SystemIdentity {
        hostname: read_trimmed(PROC_HOSTNAME),
        kernel_release: read_trimmed(PROC_KERNEL_RELEASE),
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn filesystem_usage(path: &str) -> io::Result<ByteUsage> {
    let path = CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a null byte"))?;
    let mut statistics = MaybeUninit::<libc::statvfs>::uninit();

    // SAFETY: `path` is a valid, null-terminated C string and `statistics` points
    // to writable memory for a `statvfs` value initialized by a successful call.
    if unsafe { libc::statvfs(path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `statvfs` call initialized the complete structure.
    let statistics = unsafe { statistics.assume_init() };

    let block_size = if statistics.f_frsize == 0 {
        statistics.f_bsize
    } else {
        statistics.f_frsize
    };
    let total = u128::from(statistics.f_blocks) * u128::from(block_size);
    let available = u128::from(statistics.f_bavail) * u128::from(block_size);
    let total = total.min(u128::from(u64::MAX)) as u64;
    let available = available.min(u128::from(u64::MAX)) as u64;

    Ok(ByteUsage {
        used: total.saturating_sub(available),
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aggregate_cpu_times_without_double_counting_guest_time() {
        let contents = "cpu  100 20 30 400 50 6 7 8 9 10\ncpu0 1 2 3 4\n";

        assert_eq!(
            parse_cpu_sample(contents).map(|sample| sample.aggregate),
            Some(CpuTimes {
                idle: 450,
                total: 621,
            })
        );
    }

    #[test]
    fn parses_stable_logical_cpu_identifiers_and_skips_malformed_rows() {
        let contents = concat!(
            "cpu  100 0 50 850 0 0 0 0\n",
            "cpu2 20 0 10 170 0 0 0 0\n",
            "cpu0 10 0 5 85 0 0 0 0\n",
            "cpu1 malformed\n",
            "cpuX 1 2 3 4\n",
            "intr 100\n",
        );

        let sample = parse_cpu_sample(contents).unwrap();
        let ids = sample
            .logical
            .keys()
            .map(|id| id.index())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec![0, 2]);
    }

    #[test]
    fn calculates_each_logical_cpu_from_matching_counter_deltas() {
        let previous = parse_cpu_sample(concat!(
            "cpu 100 0 0 100 0 0 0 0\n",
            "cpu0 40 0 0 60 0 0 0 0\n",
            "cpu1 60 0 0 40 0 0 0 0\n",
        ))
        .unwrap();
        let current = parse_cpu_sample(concat!(
            "cpu 160 0 0 140 0 0 0 0\n",
            "cpu0 70 0 0 70 0 0 0 0\n",
            "cpu1 90 0 0 70 0 0 0 0\n",
        ))
        .unwrap();

        let (aggregate, logical) = cpu_metrics(Some(&previous), &current);

        assert_eq!(aggregate, Some(60.0));
        assert_eq!(logical.len(), 2);
        assert_eq!(logical[0].id.index(), 0);
        assert_eq!(logical[0].utilization_percent, Some(75.0));
        assert_eq!(logical[1].id.index(), 1);
        assert_eq!(logical[1].utilization_percent, Some(50.0));
    }

    #[test]
    fn first_cpu_sample_has_identities_without_invented_utilization() {
        let current = parse_cpu_sample(concat!(
            "cpu 100 0 0 100\n",
            "cpu0 40 0 0 60\n",
            "cpu1 60 0 0 40\n",
        ))
        .unwrap();

        let (aggregate, logical) = cpu_metrics(None, &current);

        assert_eq!(aggregate, None);
        assert_eq!(logical.len(), 2);
        assert!(logical.iter().all(|cpu| cpu.utilization_percent.is_none()));
    }

    #[test]
    fn logical_cpu_appearance_and_disappearance_are_nonfatal() {
        let previous = parse_cpu_sample(concat!(
            "cpu 100 0 0 100\n",
            "cpu0 40 0 0 60\n",
            "cpu1 60 0 0 40\n",
        ))
        .unwrap();
        let current = parse_cpu_sample(concat!(
            "cpu 160 0 0 140\n",
            "cpu1 90 0 0 70\n",
            "cpu3 10 0 0 90\n",
        ))
        .unwrap();

        let (_, logical) = cpu_metrics(Some(&previous), &current);

        assert_eq!(logical.len(), 2);
        assert_eq!(logical[0].id.index(), 1);
        assert_eq!(logical[0].utilization_percent, Some(50.0));
        assert_eq!(logical[1].id.index(), 3);
        assert_eq!(logical[1].utilization_percent, None);
    }

    #[test]
    fn rejects_missing_or_incomplete_aggregate_cpu_data() {
        assert_eq!(parse_cpu_sample("cpu0 1 2 3 4\n"), None);
        assert_eq!(parse_cpu_sample("cpu 1 2 3\ncpu0 1 2 3 4\n"), None);
        assert_eq!(parse_cpu_sample("cpu 1 bad 3 4\n"), None);
    }

    #[test]
    fn calculates_cpu_usage_from_counter_deltas() {
        let previous = CpuTimes {
            idle: 400,
            total: 1_000,
        };
        let current = CpuTimes {
            idle: 450,
            total: 1_200,
        };

        assert_eq!(cpu_utilization(previous, current), Some(75.0));
    }

    #[test]
    fn rejects_invalid_cpu_deltas() {
        let sample = CpuTimes {
            idle: 400,
            total: 1_000,
        };
        assert_eq!(cpu_utilization(sample, sample), None);

        let reset = CpuTimes {
            idle: 10,
            total: 20,
        };
        assert_eq!(cpu_utilization(sample, reset), None);
    }

    #[test]
    fn parses_memory_usage_from_available_memory() {
        let contents = "MemTotal:       1000 kB\nMemAvailable:    400 kB\n";

        assert_eq!(
            parse_memory_usage(contents),
            Some(ByteUsage {
                used: 600 * 1024,
                total: 1000 * 1024,
            })
        );
    }

    /// A /proc/diskstats line with `extra` trailing fields beyond the 14 of
    /// the oldest format (4 from Linux 4.18, 6 from 5.5).
    fn diskstats_line(name: &str, sectors_read: u64, sectors_written: u64, extra: usize) -> String {
        let mut line = format!(
            "   8       0 {name} 100 5 {sectors_read} 40 200 7 {sectors_written} 90 0 120 130"
        );
        for _ in 0..extra {
            line.push_str(" 0");
        }
        line.push('\n');
        line
    }

    #[test]
    fn diskstats_keeps_whole_disks_in_every_kernel_format() {
        let contents = [
            diskstats_line("sda", 10, 20, 0),
            diskstats_line("sda1", 1, 2, 0),
            diskstats_line("nvme0n1", 30, 40, 4),
            diskstats_line("nvme0n1p2", 3, 4, 4),
            diskstats_line("vdb", 50, 60, 6),
            diskstats_line("loop0", 5, 6, 6),
            diskstats_line("zram0", 5, 6, 6),
            diskstats_line("dm-0", 5, 6, 6),
            diskstats_line("md127", 5, 6, 6),
            diskstats_line("sr0", 5, 6, 6),
        ]
        .concat();

        let rows: Vec<_> = parse_diskstats(&contents).collect();

        assert_eq!(
            rows,
            [("sda", [10, 20]), ("nvme0n1", [30, 40]), ("vdb", [50, 60])]
        );
    }

    #[test]
    fn malformed_diskstats_lines_are_skipped() {
        let max = u64::MAX;
        let contents = format!(
            "{}{}{}{}{}{}",
            "",
            "   8 0 sdb 1 2 3\n",
            "   8 0 sdc 1 2 x 4 5 6 7 8 9 10 11\n",
            "garbage\n\n",
            "   8 0 sdd -1 0 0 0 0 0 0 0 0 0 0\n",
            diskstats_line("sde", max, max, 0),
        );

        assert_eq!(
            parse_diskstats(&contents).collect::<Vec<_>>(),
            [("sde", [max, max])]
        );
        assert_eq!(parse_diskstats("").count(), 0);
    }

    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                "tuxctl_diskstats_{label}_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )))
        }

        fn write(&self, contents: &str) {
            fs::write(&self.0, contents).unwrap();
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn rates(disks: &[DiskIo]) -> Vec<(&str, Option<f64>, Option<f64>)> {
        disks
            .iter()
            .map(|disk| {
                (
                    &*disk.name,
                    disk.read_bytes_per_sec,
                    disk.write_bytes_per_sec,
                )
            })
            .collect()
    }

    #[test]
    fn disk_rates_come_from_sector_deltas_over_elapsed_time() {
        let file = TempFile::new("rates");
        let mut sampler = DiskIoSampler::default();
        let t0 = Instant::now();

        file.write(&diskstats_line("sda", 1_000, 2_000, 4));
        assert_eq!(
            rates(&sampler.collect(&file.0, t0)),
            [("sda", None, None)],
            "no rate without a previous sample"
        );

        // 4096 sectors read and 2048 written in 2 s.
        file.write(&diskstats_line("sda", 5_096, 4_048, 4));
        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(2))),
            [("sda", Some(1_048_576.0), Some(524_288.0))]
        );

        // A counter that went backwards (reset or 32-bit wrap) reads as zero once.
        file.write(&diskstats_line("sda", 10, 4_048, 4));
        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(3))),
            [("sda", Some(0.0), Some(0.0))]
        );

        // Samples less than 1 ms apart report no rate.
        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(3))),
            [("sda", None, None)]
        );
    }

    #[test]
    fn counters_near_u64_max_do_not_overflow() {
        let file = TempFile::new("overflow");
        let mut sampler = DiskIoSampler::default();
        let t0 = Instant::now();

        file.write(&diskstats_line("sda", u64::MAX - 10, u64::MAX - 1, 0));
        sampler.collect(&file.0, t0);
        file.write(&diskstats_line("sda", u64::MAX, u64::MAX, 0));

        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(1))),
            [("sda", Some(5_120.0), Some(512.0))]
        );
    }

    #[test]
    fn disappearing_disks_restart_without_a_rate_across_the_gap() {
        let file = TempFile::new("hotplug");
        let mut sampler = DiskIoSampler::default();
        let t0 = Instant::now();
        let both = [
            diskstats_line("sda", 100, 100, 0),
            diskstats_line("sdb", 100, 100, 0),
        ]
        .concat();

        file.write(&both);
        sampler.collect(&file.0, t0);
        file.write(&diskstats_line("sda", 612, 100, 0));
        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(1))),
            [("sda", Some(262_144.0), Some(0.0))],
            "sdb vanished"
        );

        file.write(
            &[
                diskstats_line("sda", 612, 100, 0),
                diskstats_line("sdb", 9_000, 9_000, 0),
            ]
            .concat(),
        );
        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(2))),
            [("sda", Some(0.0), Some(0.0)), ("sdb", None, None)],
            "a returning disk gets no rate spanning its absence"
        );
    }

    #[test]
    fn a_failed_read_clears_the_rate_baselines() {
        let file = TempFile::new("failure");
        let mut sampler = DiskIoSampler::default();
        let t0 = Instant::now();

        file.write(&diskstats_line("sda", 100, 100, 0));
        sampler.collect(&file.0, t0);
        let missing = std::path::Path::new("/nonexistent/tuxctl/diskstats");
        assert!(sampler
            .collect(missing, t0 + Duration::from_secs(1))
            .is_empty());

        file.write(&diskstats_line("sda", 900, 900, 0));
        assert_eq!(
            rates(&sampler.collect(&file.0, t0 + Duration::from_secs(2))),
            [("sda", None, None)]
        );
    }

    #[test]
    fn duplicate_disk_names_keep_the_first_row() {
        let file = TempFile::new("duplicates");
        let mut sampler = DiskIoSampler::default();
        file.write(
            &[
                diskstats_line("sda", 1, 1, 0),
                diskstats_line("sda", 2, 2, 0),
            ]
            .concat(),
        );

        assert_eq!(sampler.collect(&file.0, Instant::now()).len(), 1);
    }

    #[test]
    fn steady_state_disk_sampling_reuses_its_read_buffer() {
        let file = TempFile::new("buffer");
        let mut sampler = DiskIoSampler::default();
        let t0 = Instant::now();
        file.write(
            &[
                diskstats_line("sda", 1, 1, 6),
                diskstats_line("nvme0n1", 1, 1, 6),
            ]
            .concat(),
        );
        sampler.collect(&file.0, t0);
        let capacity = sampler.buffer.capacity();

        for second in 1..20 {
            sampler.collect(&file.0, t0 + Duration::from_secs(second));
        }

        assert_eq!(sampler.buffer.capacity(), capacity);
        assert_eq!(sampler.previous.len(), 2);
    }

    #[test]
    fn parses_uptime_and_load_average() {
        assert_eq!(
            parse_uptime("123.75 42.0\n"),
            Some(Duration::from_secs_f64(123.75))
        );
        assert_eq!(
            parse_load_average("1.24 0.98 0.72 2/100 123\n"),
            Some(LoadAverage {
                one: 1.24,
                five: 0.98,
                fifteen: 0.72,
            })
        );
    }
}
