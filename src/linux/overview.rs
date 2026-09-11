use std::{
    ffi::CString,
    fs, io,
    mem::MaybeUninit,
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

const PROC_STAT: &str = "/proc/stat";
const PROC_MEMINFO: &str = "/proc/meminfo";
const PROC_UPTIME: &str = "/proc/uptime";
const PROC_LOADAVG: &str = "/proc/loadavg";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverviewMetrics {
    pub cpu_percent: Option<f64>,
    pub memory: Option<ByteUsage>,
    pub uptime: Option<Duration>,
    pub load_average: Option<LoadAverage>,
    pub root_filesystem: Option<ByteUsage>,
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

pub struct OverviewCollector {
    receiver: Receiver<OverviewMetrics>,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl OverviewCollector {
    pub fn start(refresh_rate: Duration) -> io::Result<Self> {
        let (metrics_tx, receiver) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("overview-metrics".into())
            .spawn(move || {
                let mut previous_cpu = None;

                loop {
                    if metrics_tx.send(collect(&mut previous_cpu)).is_err() {
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

    pub fn latest(&self) -> Option<OverviewMetrics> {
        self.receiver.try_iter().last()
    }
}

impl Drop for OverviewCollector {
    fn drop(&mut self) {
        let _ = self.stop.send(());
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

fn collect(previous_cpu: &mut Option<CpuTimes>) -> OverviewMetrics {
    let current_cpu = fs::read_to_string(PROC_STAT)
        .ok()
        .and_then(|contents| parse_cpu_times(&contents));
    let cpu_percent = (*previous_cpu)
        .zip(current_cpu)
        .and_then(|(previous, current)| cpu_utilization(previous, current));
    if current_cpu.is_some() {
        *previous_cpu = current_cpu;
    }

    OverviewMetrics {
        cpu_percent,
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
    }
}

fn parse_cpu_times(contents: &str) -> Option<CpuTimes> {
    let line = contents.lines().find(|line| line.starts_with("cpu "))?;
    let values = line
        .split_whitespace()
        .skip(1)
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
            parse_cpu_times(contents),
            Some(CpuTimes {
                idle: 450,
                total: 621,
            })
        );
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
