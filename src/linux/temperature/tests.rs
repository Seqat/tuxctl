use std::{cell::RefCell, os::unix::fs::symlink, rc::Rc};

use super::*;

/// A fake /sys tree under a unique temporary directory.
struct Tree(PathBuf);

impl Tree {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "tuxctl-temperature-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }

    fn roots(&self) -> SysfsRoots {
        SysfsRoots {
            hwmon: self.0.join("class/hwmon"),
            thermal: self.0.join("class/thermal"),
            drm: self.0.join("class/drm"),
            block: self.0.join("block"),
            net: self.0.join("class/net"),
            nvidia_proc: self.0.join("proc/nvidia/gpus"),
            powercap: self.0.join("class/powercap"),
        }
    }

    fn write(&self, path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn device(&self, relative: &str) -> PathBuf {
        let path = self.0.join("devices").join(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn link(&self, target: &Path, link: &Path) {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(target, link).unwrap();
    }

    /// `temps`: (index, label, millidegrees).
    fn hwmon(
        &self,
        index: u32,
        name: &str,
        device: Option<&Path>,
        temps: &[(u32, Option<&str>, i64)],
    ) -> PathBuf {
        let path = self.0.join(format!("class/hwmon/hwmon{index}"));
        self.write(&path.join("name"), &format!("{name}\n"));
        if let Some(device) = device {
            self.link(device, &path.join("device"));
        }
        for &(temp, label, value) in temps {
            self.write(
                &path.join(format!("temp{temp}_input")),
                &format!("{value}\n"),
            );
            if let Some(label) = label {
                self.write(
                    &path.join(format!("temp{temp}_label")),
                    &format!("{label}\n"),
                );
            }
        }
        path
    }

    fn gpu(&self, card: u32, device: &Path, driver: &str) {
        let driver_path = self.0.join("drivers").join(driver);
        fs::create_dir_all(&driver_path).unwrap();
        self.link(&driver_path, &device.join("driver"));
        self.link(device, &self.0.join(format!("class/drm/card{card}/device")));
    }

    fn power(&self, device: &Path, control: &str, status: &str) {
        self.write(&device.join("power/control"), control);
        self.write(&device.join("power/runtime_status"), status);
    }

    fn disk(&self, name: &str, device: &Path) {
        self.link(device, &self.0.join(format!("block/{name}/device")));
    }

    fn nic(&self, name: &str, device: &Path) {
        self.link(device, &self.0.join(format!("class/net/{name}/device")));
    }

    fn zone(&self, index: u32, kind: &str, millidegrees: i64) {
        let zone = self.0.join(format!("class/thermal/thermal_zone{index}"));
        self.write(&zone.join("type"), kind);
        self.write(&zone.join("temp"), &millidegrees.to_string());
    }

    fn sampler(&self) -> TemperatureSampler<NoNvidia> {
        TemperatureSampler::new(self.roots(), Some(NoNvidia), Instant::now())
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn device_key(path: &Path) -> TemperatureKey {
    TemperatureKey::Device(Arc::from(path))
}

fn values<N: NvidiaSource>(
    sampler: &mut TemperatureSampler<N>,
) -> Vec<(TemperatureKey, Option<i16>)> {
    sampler
        .sample(Instant::now())
        .iter()
        .map(|temperature| (temperature.key.clone(), temperature.celsius))
        .collect()
}

#[test]
fn coretemp_packages_map_to_their_physical_ids() {
    let tree = Tree::new("coretemp");
    let package0 = tree.device("platform/coretemp.0");
    let package1 = tree.device("platform/coretemp.1");
    tree.hwmon(
        3,
        "coretemp",
        Some(&package0),
        &[
            (1, Some("Package id 0"), 51_000),
            (2, Some("Core 0"), 49_000),
        ],
    );
    let hwmon = tree.hwmon(
        4,
        "coretemp",
        Some(&package1),
        &[
            (1, Some("Package id 1"), 63_600),
            (2, Some("Core 0"), 60_000),
        ],
    );
    tree.write(&hwmon.join("temp1_max"), "80000");
    tree.write(&hwmon.join("temp1_crit"), "100000");

    let mut sampler = tree.sampler();
    let temperatures = sampler.sample(Instant::now()).to_vec();
    assert_eq!(
        temperatures,
        [
            Temperature {
                key: TemperatureKey::CpuPackage(0),
                celsius: Some(51),
                max: None,
                crit: None,
            },
            Temperature {
                key: TemperatureKey::CpuPackage(1),
                celsius: Some(64),
                max: Some(80),
                crit: Some(100),
            },
        ]
    );
}

#[test]
fn coretemp_without_a_package_sensor_uses_its_hottest_core() {
    let tree = Tree::new("coretemp-cores");
    let device = tree.device("platform/coretemp.1");
    tree.hwmon(
        0,
        "coretemp",
        Some(&device),
        &[
            (2, Some("Core 0"), 48_000),
            (3, Some("Core 1"), 57_400),
            (4, Some("Core 2"), 52_000),
        ],
    );

    assert_eq!(
        values(&mut tree.sampler()),
        [(TemperatureKey::CpuPackage(1), Some(57))]
    );
}

#[test]
fn via_cputemp_cores_form_one_package() {
    let tree = Tree::new("via");
    tree.hwmon(0, "via_cputemp", None, &[(1, Some("Core 0"), 40_000)]);
    tree.hwmon(1, "via_cputemp", None, &[(1, Some("Core 1"), 44_000)]);

    assert_eq!(
        values(&mut tree.sampler()),
        [(TemperatureKey::CpuPackage(0), Some(44))]
    );
}

#[test]
fn k10temp_prefers_tdie_then_tctl_and_ignores_ccd_sensors() {
    let tree = Tree::new("k10temp");
    let node0 = tree.device("pci0000:00/0000:00:18.3");
    let node1 = tree.device("pci0000:00/0000:00:19.3");
    // Listed out of node order: hwmon numbering is not the package order.
    tree.hwmon(
        1,
        "k10temp",
        Some(&node1),
        &[(1, Some("Tctl"), 71_000), (3, Some("Tccd1"), 90_000)],
    );
    tree.hwmon(
        2,
        "k10temp",
        Some(&node0),
        &[
            (1, Some("Tctl"), 70_000),
            (2, Some("Tdie"), 60_000),
            (3, Some("Tccd1"), 95_000),
        ],
    );

    let mut readings = values(&mut tree.sampler());
    readings.sort_by_key(|(key, _)| format!("{key:?}"));
    assert_eq!(
        readings,
        [
            (TemperatureKey::CpuPackage(0), Some(60)),
            (TemperatureKey::CpuPackage(1), Some(71)),
        ]
    );
}

#[test]
fn zenpower_with_only_tctl_uses_it() {
    let tree = Tree::new("zenpower");
    let node = tree.device("pci0000:00/0000:00:18.3");
    tree.hwmon(
        0,
        "zenpower",
        Some(&node),
        &[(1, Some("Tctl"), 45_500), (3, Some("Tccd1"), 50_000)],
    );

    assert_eq!(
        values(&mut tree.sampler()),
        [(TemperatureKey::CpuPackage(0), Some(46))]
    );
}

#[test]
fn amdgpu_prefers_the_edge_sensor_over_junction() {
    let tree = Tree::new("amdgpu");
    let gpu = tree.device("pci0000:00/0000:03:00.0");
    tree.gpu(0, &gpu, "amdgpu");
    tree.power(&gpu, "auto", "active");
    let hwmon = tree.hwmon(
        5,
        "amdgpu",
        Some(&gpu),
        &[
            (1, Some("junction"), 70_000),
            (2, Some("edge"), 55_000),
            (3, Some("mem"), 60_000),
        ],
    );
    tree.write(&hwmon.join("temp2_crit"), "100000");

    let mut sampler = tree.sampler();
    let temperatures = sampler.sample(Instant::now()).to_vec();
    assert_eq!(temperatures.len(), 1);
    assert_eq!(temperatures[0].key, device_key(&gpu));
    assert_eq!(temperatures[0].celsius, Some(55));
    assert_eq!(temperatures[0].crit, Some(100));
}

#[test]
fn nouveau_uses_its_first_sensor() {
    let tree = Tree::new("nouveau");
    let gpu = tree.device("pci0000:00/0000:01:00.0");
    tree.gpu(1, &gpu, "nouveau");
    tree.hwmon(0, "nouveau", Some(&gpu), &[(1, None, 42_000)]);

    assert_eq!(values(&mut tree.sampler()), [(device_key(&gpu), Some(42))]);
}

#[test]
fn intel_igpu_without_its_own_sensor_shows_nothing() {
    let tree = Tree::new("i915");
    let gpu = tree.device("pci0000:00/0000:00:02.0");
    tree.gpu(0, &gpu, "i915");
    // i915 registers energy/power hwmon attributes without temperatures.
    let hwmon = tree.hwmon(0, "i915", Some(&gpu), &[]);
    tree.write(&hwmon.join("energy1_input"), "123");

    assert!(values(&mut tree.sampler()).is_empty());
}

#[test]
fn nvme_matches_by_path_and_falls_back_to_the_controller_name() {
    let tree = Tree::new("nvme");
    // Path match: hwmon on the controller, as is the disk's device.
    let controller0 = tree.device("pci0000:00/0000:02:00.0/nvme/nvme0");
    tree.disk("nvme0n1", &controller0);
    tree.hwmon(
        0,
        "nvme",
        Some(&controller0),
        &[
            (1, Some("Composite"), 38_850),
            (2, Some("Sensor 1"), 50_000),
        ],
    );
    // Name fallback: hwmon on the PCI function, disk on a multipath subsystem.
    let pci = tree.device("pci0000:00/0000:05:00.0");
    tree.device("pci0000:00/0000:05:00.0/nvme/nvme1");
    let subsystem = tree.device("virtual/nvme-subsystem/nvme-subsys1");
    tree.disk("nvme1n1", &subsystem);
    tree.disk("nvme1n2", &subsystem);
    tree.hwmon(1, "nvme", Some(&pci), &[(1, Some("Composite"), 45_000)]);

    assert_eq!(
        values(&mut tree.sampler()),
        [
            (device_key(&controller0), Some(39)),
            (device_key(&subsystem), Some(45))
        ]
    );
}

#[test]
fn nvme_hwmon_named_after_its_controller_matches_its_namespaces() {
    let tree = Tree::new("nvme-name");
    let controller = tree.device("pci0000:00/0000:02:00.0/nvme/nvme3");
    let namespace = tree.device("pci0000:00/0000:02:00.0/nvme/nvme3/ns1");
    tree.disk("nvme3n1", &namespace);
    tree.hwmon(
        0,
        "nvme",
        Some(&controller),
        &[(1, Some("Composite"), 40_000)],
    );

    assert_eq!(
        values(&mut tree.sampler()),
        [(device_key(&namespace), Some(40))]
    );
}

#[test]
fn drivetemp_matches_its_disk_and_other_disks_get_nothing() {
    let tree = Tree::new("drivetemp");
    let sda = tree.device("pci0000:00/ata1/host0/target0:0:0/0:0:0:0");
    let sdb = tree.device("pci0000:00/ata2/host1/target1:0:0/1:0:0:0");
    tree.disk("sda", &sda);
    tree.disk("sdb", &sdb);
    tree.hwmon(0, "drivetemp", Some(&sda), &[(1, None, 33_000)]);

    assert_eq!(values(&mut tree.sampler()), [(device_key(&sda), Some(33))]);
}

#[test]
fn nic_sensor_on_a_child_device_belongs_to_the_nic() {
    let tree = Tree::new("nic");
    let nic = tree.device("pci0000:00/0000:06:00.0");
    let phy = tree.device("pci0000:00/0000:06:00.0/mdio_bus/r8169-0-600/r8169-0-600:00");
    tree.nic("enp6s0", &nic);
    let hwmon = tree.hwmon(3, "r8169_0_600:00", Some(&phy), &[(1, None, 47_000)]);
    tree.write(&hwmon.join("temp1_max"), "120000");
    // An unrelated hwmon (RAM SPD) matches nothing.
    let spd = tree.device("pci0000:00/0000:00:14.0/i2c-6/6-0051");
    tree.hwmon(4, "spd5118", Some(&spd), &[(1, None, 40_000)]);

    let mut sampler = tree.sampler();
    let temperatures = sampler.sample(Instant::now()).to_vec();
    assert_eq!(temperatures.len(), 1);
    assert_eq!(temperatures[0].key, device_key(&nic));
    assert_eq!(
        (temperatures[0].celsius, temperatures[0].max),
        (Some(47), Some(120))
    );
}

#[test]
fn hwmon_without_a_device_link_matches_no_device() {
    let tree = Tree::new("no-device");
    let disk = tree.device("pci0000:00/0000:02:00.0/nvme/nvme0");
    tree.disk("nvme0n1", &disk);
    tree.hwmon(0, "nvme", None, &[(1, Some("Composite"), 40_000)]);
    tree.hwmon(1, "acpitz", None, &[(1, None, 27_800)]);

    assert!(values(&mut tree.sampler()).is_empty());
}

#[test]
fn thermal_zones_stand_in_for_a_cpu_without_hwmon_but_never_acpitz() {
    let tree = Tree::new("thermal");
    tree.zone(0, "acpitz", 90_000);
    tree.zone(1, "cpu-thermal", 51_000);
    tree.zone(2, "soc_thermal", 53_000);
    tree.zone(3, "gpu-thermal", 70_000);

    assert_eq!(
        values(&mut tree.sampler()),
        [(TemperatureKey::CpuPackage(0), Some(53))]
    );

    let acpi_only = Tree::new("thermal-acpi");
    acpi_only.zone(0, "acpitz", 40_000);
    assert!(values(&mut acpi_only.sampler()).is_empty());

    let x86 = Tree::new("thermal-x86");
    x86.zone(0, "x86_pkg_temp", 44_000);
    assert_eq!(
        values(&mut x86.sampler()),
        [(TemperatureKey::CpuPackage(0), Some(44))]
    );
}

#[test]
fn thermal_zones_are_ignored_when_a_cpu_hwmon_exists() {
    let tree = Tree::new("thermal-hwmon");
    tree.zone(0, "x86_pkg_temp", 90_000);
    tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 50_000)]);

    assert_eq!(
        values(&mut tree.sampler()),
        [(TemperatureKey::CpuPackage(0), Some(50))]
    );
}

#[test]
fn readings_convert_rounded_and_reject_implausible_values() {
    assert_eq!(celsius_from_millidegrees(38_499), Some(38));
    assert_eq!(celsius_from_millidegrees(38_500), Some(39));
    assert_eq!(celsius_from_millidegrees(-12_500), Some(-13));
    assert_eq!(celsius_from_millidegrees(-40_000), Some(-40));
    assert_eq!(celsius_from_millidegrees(150_000), Some(150));
    assert_eq!(celsius_from_millidegrees(-41_000), None);
    assert_eq!(celsius_from_millidegrees(151_000), None);
    assert_eq!(celsius_from_millidegrees(i64::MAX), None);
    assert_eq!(celsius_from_millidegrees(i64::MIN), None);

    let tree = Tree::new("invalid");
    let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 255_000)]);
    let mut sampler = tree.sampler();
    assert_eq!(
        values(&mut sampler),
        [(TemperatureKey::CpuPackage(0), None)]
    );
    tree.write(&hwmon.join("temp1_input"), "garbage");
    let later = Instant::now() + READ_INTERVAL;
    assert_eq!(sampler.sample(later)[0].celsius, None);
    assert!(
        !sampler.rediscover,
        "an invalid value is not a read failure"
    );
}

#[test]
fn nvidia_rtd3_policy_keeps_nvml_open_only_when_the_gpu_cannot_suspend() {
    let status =
        |value: &str| format!("Runtime D3 status:          {value}\nVideo Memory:  Active\n");
    for (control, proc_power, keeps_open) in [
        (Some("on"), None, true),
        (Some("on"), Some(status("Enabled (fine-grained)")), true),
        (Some("auto"), Some(status("Not supported")), true),
        (Some("auto"), Some(status("Disabled by default")), true),
        (Some("auto"), Some(status("Disabled")), true),
        (Some("auto"), Some(status("Enabled (fine-grained)")), false),
        (
            Some("auto"),
            Some(status("Enabled (coarse-grained)")),
            false,
        ),
        (Some("auto"), Some(status("Something new")), false),
        (Some("auto"), Some("Video Memory: Active\n".into()), false),
        (Some("auto"), None, false),
        (None, None, false),
    ] {
        assert_eq!(
            nvidia_keeps_open(control, proc_power.as_deref()),
            keeps_open,
            "{control:?} {proc_power:?}"
        );
    }
}

#[test]
fn nvidia_rtd3_policy_is_read_from_the_driver_at_discovery() {
    let tree = Tree::new("nvidia-policy");
    let gpu = tree.device("pci0000:00/0000:01:00.0");
    tree.gpu(1, &gpu, "nvidia");
    tree.power(&gpu, "auto", "active");
    let policy = |tree: &Tree| {
        discover(&tree.roots(), true)
            .sensors
            .into_iter()
            .find_map(|sensor| match sensor.source {
                Source::Nvidia {
                    keep_open, bus_id, ..
                } => Some((bus_id, keep_open)),
                _ => None,
            })
    };

    assert_eq!(
        policy(&tree),
        Some(("0000:01:00.0".into(), false)),
        "missing file"
    );
    tree.write(
        &tree.0.join("proc/nvidia/gpus/0000:01:00.0/power"),
        "Runtime D3 status:          Not supported\n",
    );
    assert_eq!(policy(&tree), Some(("0000:01:00.0".into(), true)));
}

fn at(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
}

/// Sample times at `interval` (ms) with alternating `jitter` (ms), and
/// which of those samples read the sensors.
fn reads(interval: u64, jitter: i64, samples: usize) -> Vec<bool> {
    let tree = Tree::new("cadence");
    let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 40_000)]);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
    (0..samples)
        .map(|index| {
            let offset = if index % 2 == 0 { jitter } else { -jitter };
            let millis = (interval * index as u64).saturating_add_signed(if index == 0 {
                0
            } else {
                offset
            });
            // A new value each sample shows whether this sample read it.
            let value = 40_000 + 1_000 * index as i64;
            tree.write(&hwmon.join("temp1_input"), &value.to_string());
            sampler.sample(at(start, millis))[0].celsius == Some(40 + index as i16)
        })
        .collect()
}

#[test]
fn temperatures_are_read_at_most_every_two_seconds_whatever_the_interval() {
    let every = |n: usize, count: usize| (0..count).map(|index| index % n == 0).collect::<Vec<_>>();
    assert_eq!(reads(250, 0, 17), every(8, 17));
    assert_eq!(reads(1_000, 0, 7), every(2, 7));
    assert_eq!(reads(60_000, 0, 3), every(1, 3));
}

#[test]
fn timer_jitter_at_a_two_second_interval_does_not_skip_reads() {
    assert!(reads(2_000, 50, 8).into_iter().all(|read| read));
    assert!(reads(2_000, -50, 8).into_iter().all(|read| read));
}

#[test]
fn values_carry_over_between_reads_and_follow_interval_changes() {
    let tree = Tree::new("carry");
    let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 40_000)]);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
    let mut sample = |millis: u64, value: i64| {
        tree.write(&hwmon.join("temp1_input"), &value.to_string());
        sampler.sample(at(start, millis))[0].celsius
    };

    assert_eq!(sample(0, 40_000), Some(40));
    assert_eq!(sample(250, 41_000), Some(40), "carried over");
    assert_eq!(sample(500, 42_000), Some(40), "carried over");
    // The interval switches from 250 ms to 60 s: every sample reads.
    assert_eq!(sample(60_500, 43_000), Some(43));
    assert_eq!(sample(120_500, 44_000), Some(44));
    // And back to 1 s.
    assert_eq!(sample(121_500, 45_000), Some(44));
    assert_eq!(sample(122_500, 46_000), Some(46));
}

#[test]
fn a_failing_sensor_triggers_rate_limited_rediscovery() {
    let tree = Tree::new("rediscover");
    let device = tree.device("pci0000:00/0000:02:00.0/nvme/nvme0");
    tree.disk("nvme0n1", &device);
    let hwmon = tree.hwmon(0, "nvme", Some(&device), &[(1, Some("Composite"), 40_000)]);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
    assert_eq!(sampler.sample(start)[0].celsius, Some(40));

    // The driver re-registers its hwmon under a new number.
    fs::remove_dir_all(&hwmon).unwrap();
    tree.hwmon(7, "nvme", Some(&device), &[(1, Some("Composite"), 41_000)]);
    assert_eq!(sampler.sample(at(start, 2_000))[0].celsius, None);
    assert!(sampler.rediscover);
    // Not before 30 s have passed since the last discovery.
    assert_eq!(sampler.sample(at(start, 28_000))[0].celsius, None);
    assert_eq!(sampler.sample(at(start, 30_000))[0].celsius, Some(41));
    assert!(!sampler.rediscover);
}

#[test]
fn a_sensor_that_never_worked_does_not_trigger_rediscovery() {
    let tree = Tree::new("never-worked");
    let hwmon = tree.hwmon(0, "coretemp", None, &[(1, Some("Package id 0"), 40_000)]);
    fs::remove_file(hwmon.join("temp1_input")).unwrap();
    fs::create_dir(hwmon.join("temp1_input")).unwrap(); // listed, but unreadable
    let mut sampler = tree.sampler();

    assert_eq!(
        values(&mut sampler),
        [(TemperatureKey::CpuPackage(0), None)]
    );
    assert!(!sampler.rediscover);
}

#[test]
fn a_suspended_gpu_is_not_read_and_does_not_trigger_rediscovery() {
    let tree = Tree::new("suspended");
    let gpu = tree.device("pci0000:00/0000:03:00.0");
    tree.gpu(0, &gpu, "amdgpu");
    tree.power(&gpu, "auto", "active");
    let hwmon = tree.hwmon(0, "amdgpu", Some(&gpu), &[(1, Some("edge"), 50_000)]);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);
    assert_eq!(sampler.sample(start)[0].celsius, Some(50));

    // Suspended: amdgpu refuses reads (EPERM); here the file is gone.
    tree.write(&gpu.join("power/runtime_status"), "suspended");
    fs::remove_file(hwmon.join("temp1_input")).unwrap();
    assert_eq!(sampler.sample(at(start, 2_000))[0].celsius, None);
    assert!(!sampler.rediscover);

    tree.write(&gpu.join("power/runtime_status"), "active");
    tree.write(&hwmon.join("temp1_input"), "52000");
    assert_eq!(sampler.sample(at(start, 4_000))[0].celsius, Some(52));
}

/// Bus ids asked for and the `keep_open` flag, per call.
type NvidiaCalls = Rc<RefCell<Vec<(Vec<String>, bool)>>>;

/// Records what the sampler asks of NVML.
#[derive(Clone, Default)]
struct FakeNvidia {
    calls: NvidiaCalls,
    releases: Rc<RefCell<usize>>,
    unloads: Rc<RefCell<usize>>,
    reading: Rc<RefCell<NvidiaReading>>,
    /// Whether each call asked for a full reading.
    full_reads: Rc<RefCell<Vec<bool>>>,
}

impl FakeNvidia {
    fn reporting(celsius: i64) -> Self {
        let fake = Self::default();
        fake.reading.borrow_mut().temperature = Some(celsius);
        fake
    }
}

impl NvidiaSource for FakeNvidia {
    fn read(
        &mut self,
        bus_ids: &[&str],
        full: bool,
        keep_open: bool,
        _now: Instant,
    ) -> Vec<NvidiaReading> {
        self.full_reads.borrow_mut().push(full);
        self.calls.borrow_mut().push((
            bus_ids.iter().map(|id| (*id).to_owned()).collect(),
            keep_open,
        ));
        vec![*self.reading.borrow(); bus_ids.len()]
    }

    fn release(&mut self) {
        *self.releases.borrow_mut() += 1;
    }

    fn unload(&mut self) {
        *self.unloads.borrow_mut() += 1;
    }
}

fn nvidia_tree(label: &str, control: &str, status: &str) -> (Tree, PathBuf) {
    let tree = Tree::new(label);
    let gpu = tree.device("pci0000:00/0000:01:00.0");
    tree.gpu(1, &gpu, "nvidia");
    tree.power(&gpu, control, status);
    (tree, gpu)
}

#[test]
fn nvml_is_never_used_without_an_nvidia_gpu() {
    let tree = Tree::new("no-nvidia");
    let gpu = tree.device("pci0000:00/0000:03:00.0");
    tree.gpu(0, &gpu, "amdgpu");
    tree.hwmon(0, "amdgpu", Some(&gpu), &[(1, Some("edge"), 50_000)]);
    let fake = FakeNvidia::default();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), Instant::now());

    sampler.sample(Instant::now());
    assert!(fake.calls.borrow().is_empty());
    assert_eq!(*fake.releases.borrow(), 0);
}

#[test]
fn nvidia_gpus_get_no_entry_while_nvidia_temperatures_are_off() {
    let (tree, _) = nvidia_tree("nvidia-off", "on", "active");
    let mut sampler: TemperatureSampler<FakeNvidia> =
        TemperatureSampler::new(tree.roots(), None, Instant::now());

    assert!(values(&mut sampler).is_empty());
    assert!(discover(&tree.roots(), false).sensors.is_empty());
}

#[test]
fn nvidia_gpu_that_cannot_suspend_keeps_nvml_open() {
    let (tree, gpu) = nvidia_tree("nvidia-on", "on", "active");
    let fake = FakeNvidia::reporting(52);
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), Instant::now());

    assert_eq!(values(&mut sampler), [(device_key(&gpu), Some(52))]);
    assert_eq!(
        *fake.calls.borrow(),
        [(vec!["0000:01:00.0".to_owned()], true)]
    );
}

#[test]
fn nvidia_gpu_with_rtd3_is_read_per_cycle_and_never_woken() {
    let (tree, gpu) = nvidia_tree("nvidia-rtd3", "auto", "active");
    let fake = FakeNvidia::reporting(48);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);

    assert_eq!(sampler.sample(start)[0].celsius, Some(48));
    assert_eq!(
        *fake.calls.borrow(),
        [(vec!["0000:01:00.0".to_owned()], false)]
    );

    tree.write(&gpu.join("power/runtime_status"), "suspended");
    assert_eq!(
        sampler.sample(at(start, 2_000))[0].celsius,
        None,
        "shown as –"
    );
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "NVML not called while suspended"
    );
    assert_eq!(*fake.releases.borrow(), 1);
    assert!(!sampler.rediscover);
}

#[test]
fn nvml_values_are_validated_and_unavailable_nvml_shows_a_known_sensor() {
    let (tree, gpu) = nvidia_tree("nvidia-values", "on", "active");
    let mut unavailable = TemperatureSampler::new(tree.roots(), Some(NoNvidia), Instant::now());
    assert_eq!(values(&mut unavailable), [(device_key(&gpu), None)]);

    let implausible = FakeNvidia::reporting(4_000);
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(implausible), Instant::now());
    assert_eq!(values(&mut sampler), [(device_key(&gpu), None)]);
}

#[test]
fn the_limit_is_the_critical_temperature_else_the_maximum() {
    let temperature = |max, crit| Temperature {
        key: TemperatureKey::CpuPackage(0),
        celsius: Some(50),
        max,
        crit,
    };
    assert_eq!(temperature(Some(80), Some(90)).limit(), Some(90));
    assert_eq!(temperature(Some(80), None).limit(), Some(80));
    assert_eq!(temperature(None, None).limit(), None);
}

#[test]
fn open_nvml_refreshes_telemetry_every_sample_and_temperatures_every_two_seconds() {
    let (tree, gpu) = nvidia_tree("nvidia-telemetry", "on", "active");
    let fake = FakeNvidia::default();
    *fake.reading.borrow_mut() = NvidiaReading {
        temperature: Some(50),
        slowdown: Some(90),
        utilization: Some(30),
        memory: Some((4 << 30, 16 << 30)),
        power_milliwatts: Some(120_500),
        fan_percent: Some(40),
    };
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);

    let temperature = sampler.sample(start)[0].clone();
    assert_eq!(
        (temperature.celsius, temperature.max),
        (Some(50), Some(90)),
        "slowdown as max"
    );
    assert_eq!(
        sampler.gpus(),
        [GpuTelemetry {
            device_path: Arc::from(gpu.as_path()),
            utilization: Some(30.0),
            vram: Some(ByteUsage {
                used: 4 << 30,
                total: 16 << 30
            }),
            power_watts: Some(120.5),
            fan_percent: Some(40.0),
        }]
    );

    {
        let mut reading = fake.reading.borrow_mut();
        reading.temperature = Some(60);
        reading.utilization = Some(90);
        reading.fan_percent = Some(70);
    }
    assert_eq!(
        sampler.sample(at(start, 1_000))[0].celsius,
        Some(50),
        "temperature carried"
    );
    assert_eq!(
        sampler.gpus()[0].utilization,
        Some(90.0),
        "utilization refreshed"
    );
    assert_eq!(sampler.gpus()[0].fan_percent, Some(40.0), "fan carried");
    assert_eq!(sampler.sample(at(start, 2_000))[0].celsius, Some(60));
    assert_eq!(sampler.gpus()[0].fan_percent, Some(70.0));
    assert_eq!(fake.calls.borrow().len(), 3);
    // The costly queries (temperature, memory, fan) only every 2 s.
    assert_eq!(*fake.full_reads.borrow(), [true, false, true]);
}

#[test]
fn nvml_initialized_per_reading_follows_the_temperature_cadence() {
    let (tree, _) = nvidia_tree("nvidia-rtd3-cadence", "auto", "active");
    let fake = FakeNvidia::reporting(45);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
    for millis in [0, 250, 500, 1_000, 1_500, 2_000] {
        sampler.sample(at(start, millis));
    }
    assert_eq!(fake.calls.borrow().len(), 2, "at 0 and 2 s only");
}

#[test]
fn kernel_driver_gpus_report_telemetry_while_awake() {
    let tree = Tree::new("amdgpu-telemetry");
    let gpu = tree.device("pci0000:00/0000:03:00.0");
    tree.gpu(0, &gpu, "amdgpu");
    tree.power(&gpu, "auto", "active");
    tree.write(&gpu.join("gpu_busy_percent"), "37");
    let hwmon = tree.hwmon(0, "amdgpu", Some(&gpu), &[(1, Some("edge"), 50_000)]);
    tree.write(&hwmon.join("power1_average"), "80000000");
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);

    sampler.sample(start);
    assert_eq!(sampler.gpus()[0].utilization, Some(37.0));
    assert_eq!(sampler.gpus()[0].power_watts, Some(80.0));
    tree.write(&gpu.join("gpu_busy_percent"), "64");
    sampler.sample(at(start, 250));
    assert_eq!(sampler.gpus()[0].utilization, Some(64.0), "every sample");

    tree.write(&gpu.join("power/runtime_status"), "suspended");
    sampler.sample(at(start, 500));
    assert_eq!(
        sampler.gpus(),
        [GpuTelemetry::unavailable(Arc::from(gpu.as_path()))],
        "a suspended GPU is not read"
    );
}

#[test]
fn nvidia_gpus_have_no_telemetry_while_nvidia_temperatures_are_off() {
    let (tree, _) = nvidia_tree("nvidia-off-telemetry", "on", "active");
    let mut sampler: TemperatureSampler<FakeNvidia> =
        TemperatureSampler::new(tree.roots(), None, Instant::now());
    sampler.sample(Instant::now());
    assert!(sampler.gpus().is_empty());
}

#[test]
fn zenpower_reports_package_power_as_core_plus_soc() {
    let tree = Tree::new("zenpower-power");
    let node = tree.device("pci0000:00/0000:00:18.3");
    let hwmon = tree.hwmon(0, "zenpower", Some(&node), &[(1, Some("Tctl"), 45_000)]);
    tree.write(&hwmon.join("power1_input"), "38250000");
    tree.write(&hwmon.join("power2_input"), "11750000");
    let mut sampler = tree.sampler();
    sampler.sample(Instant::now());
    assert_eq!(sampler.cpu_power_watts(), Some(50.0));

    let k10temp = Tree::new("k10temp-power");
    let node = k10temp.device("pci0000:00/0000:00:18.3");
    k10temp.hwmon(0, "k10temp", Some(&node), &[(1, Some("Tctl"), 45_000)]);
    let mut sampler = k10temp.sampler();
    sampler.sample(Instant::now());
    assert_eq!(sampler.cpu_power_watts(), None, "k10temp has no power");
}

fn rapl_domain(tree: &Tree, domain: &str, name: &str, energy: u64) -> PathBuf {
    let path = tree.0.join("class/powercap").join(domain);
    tree.write(&path.join("name"), name);
    tree.write(&path.join("energy_uj"), &energy.to_string());
    tree.write(&path.join("max_energy_range_uj"), "262143328850");
    path.join("energy_uj")
}

#[test]
fn readable_rapl_packages_give_power_from_energy_deltas() {
    let tree = Tree::new("rapl");
    let energy = rapl_domain(&tree, "intel-rapl:0", "package-0", 1_000_000);
    // A subdomain is part of the package; counting it would count twice.
    rapl_domain(&tree, "intel-rapl:0:0", "core", 500_000);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(NoNvidia), start);

    sampler.sample(start);
    assert_eq!(
        sampler.cpu_power_watts(),
        None,
        "no delta on the first read"
    );
    tree.write(&energy, "51000000");
    sampler.sample(at(start, 2_000));
    assert_eq!(sampler.cpu_power_watts(), Some(25.0), "50 J in 2 s");

    // The counter wraps at max_energy_range_uj.
    tree.write(&energy, &(262_143_328_850_u64 - 1_000_000).to_string());
    sampler.sample(at(start, 4_000));
    tree.write(&energy, "9000000");
    sampler.sample(at(start, 6_000));
    assert_eq!(sampler.cpu_power_watts(), Some(5.0), "10 J across the wrap");
}

#[test]
fn unreadable_rapl_counters_show_no_power_and_zenpower_comes_first() {
    let tree = Tree::new("rapl-unreadable");
    let path = tree.0.join("class/powercap/intel-rapl:0");
    tree.write(&path.join("name"), "package-0");
    // Standing in for a root-only file: listed, but it cannot be read.
    fs::create_dir_all(path.join("energy_uj")).unwrap();
    assert_eq!(
        discover(&tree.roots(), false).cpu_power,
        CpuPowerSource::None
    );

    let both = Tree::new("rapl-and-zenpower");
    rapl_domain(&both, "intel-rapl:0", "package-0", 1);
    let node = both.device("pci0000:00/0000:00:18.3");
    let hwmon = both.hwmon(0, "zenpower", Some(&node), &[(1, Some("Tctl"), 45_000)]);
    both.write(&hwmon.join("power1_input"), "20000000");
    assert!(matches!(
        discover(&both.roots(), false).cpu_power,
        CpuPowerSource::Hwmon(_)
    ));
}

#[test]
fn an_inactive_sampler_reads_nothing_and_reads_at_once_when_active_again() {
    let (tree, gpu) = nvidia_tree("nvidia-inactive", "on", "active");
    let hwmon = tree.hwmon(0, "k10temp", None, &[(1, Some("Tctl"), 40_000)]);
    let fake = FakeNvidia::reporting(50);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
    let values = |sampler: &mut TemperatureSampler<FakeNvidia>, millis| {
        sampler
            .sample(at(start, millis))
            .iter()
            .map(|temperature| temperature.celsius)
            .collect::<Vec<_>>()
    };
    assert_eq!(values(&mut sampler, 0), [Some(40), Some(50)]);
    assert_eq!(fake.calls.borrow().len(), 1);

    sampler.set_active(false, at(start, 500));
    tree.write(&hwmon.join("temp1_input"), "45000");
    *fake.reading.borrow_mut() = NvidiaReading {
        temperature: Some(55),
        ..NvidiaReading::default()
    };
    for millis in [1_000, 2_000, 10_000] {
        assert_eq!(values(&mut sampler, millis), [Some(40), Some(50)], "kept");
    }
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "NVML untouched while inactive"
    );
    assert_eq!(sampler.gpus()[0].device_path, Arc::from(gpu.as_path()));

    // Active again 100 ms after the last inactive call: read at once,
    // without waiting for the 2 s gate.
    sampler.set_active(true, at(start, 10_100));
    assert_eq!(values(&mut sampler, 10_100), [Some(45), Some(55)]);
    assert_eq!(fake.calls.borrow().len(), 2);
}

#[test]
fn open_nvml_is_unloaded_once_after_the_overview_is_hidden_for_the_grace_period() {
    let (tree, _) = nvidia_tree("nvidia-unload", "on", "active");
    let fake = FakeNvidia::reporting(50);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
    sampler.sample(start);
    let grace = NVML_RELEASE_GRACE.as_millis() as u64;

    sampler.set_active(false, at(start, 1_000));
    sampler.sample(at(start, 1_000));
    sampler.sample(at(start, 1_000 + grace - 1));
    assert_eq!(*fake.unloads.borrow(), 0, "not within the grace period");
    sampler.sample(at(start, 1_000 + grace));
    assert_eq!(*fake.unloads.borrow(), 1);
    for later in [1, 60_000, 600_000] {
        sampler.sample(at(start, 1_000 + grace + later));
    }
    assert_eq!(*fake.unloads.borrow(), 1, "once per time hidden");
    assert_eq!(fake.calls.borrow().len(), 1, "no reads while hidden");
    assert_eq!(
        sampler.sample(at(start, 700_000))[0].celsius,
        Some(50),
        "kept"
    );

    // Shown again: read at once, still kept open.
    sampler.set_active(true, at(start, 700_000));
    sampler.sample(at(start, 700_000));
    assert_eq!(
        fake.calls.borrow().last().map(|(_, open)| *open),
        Some(true)
    );
    assert_eq!(fake.calls.borrow().len(), 2);

    // Hidden again: a new grace period, a second unload.
    sampler.set_active(false, at(start, 701_000));
    sampler.sample(at(start, 701_000 + grace));
    assert_eq!(*fake.unloads.borrow(), 2);
}

#[test]
fn returning_within_the_grace_period_keeps_nvml_loaded() {
    let (tree, _) = nvidia_tree("nvidia-unload-flip", "on", "active");
    let fake = FakeNvidia::reporting(50);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
    sampler.sample(start);
    let grace = NVML_RELEASE_GRACE.as_millis() as u64;

    for round in 0..3 {
        let left = 1_000 + round * grace;
        sampler.set_active(false, at(start, left));
        sampler.sample(at(start, left + grace - 1));
        sampler.set_active(true, at(start, left + grace - 1));
        sampler.sample(at(start, left + grace - 1));
    }
    // Active samples never unload, however long ago the last hiding began.
    sampler.sample(at(start, 100_000));
    assert_eq!(*fake.unloads.borrow(), 0);
    assert_eq!(*fake.releases.borrow(), 0);
}

#[test]
fn nvml_initialized_per_reading_is_unloaded_while_hidden_without_a_read() {
    let (tree, gpu) = nvidia_tree("nvidia-rtd3-hidden", "auto", "active");
    let fake = FakeNvidia::reporting(45);
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
    sampler.sample(start);
    assert_eq!(
        *fake.calls.borrow(),
        [(vec!["0000:01:00.0".to_owned()], false)]
    );
    let grace = NVML_RELEASE_GRACE.as_millis() as u64;

    // Hidden, and the GPU suspends: unloaded once, never read.
    sampler.set_active(false, at(start, 1_000));
    tree.write(&gpu.join("power/runtime_status"), "suspended");
    for millis in [1_000, 1_000 + grace, 60_000, 600_000] {
        sampler.sample(at(start, millis));
    }
    assert_eq!(*fake.unloads.borrow(), 1);
    assert_eq!(fake.calls.borrow().len(), 1, "no reads while hidden");

    // Shown while still suspended: not read either, so not loaded again.
    sampler.set_active(true, at(start, 600_000));
    sampler.sample(at(start, 600_000));
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "a suspended GPU is never read"
    );
    assert_eq!(sampler.sample(at(start, 602_000))[0].celsius, None);

    // Awake again: read per cycle, as before.
    tree.write(&gpu.join("power/runtime_status"), "active");
    sampler.sample(at(start, 604_000));
    assert_eq!(
        fake.calls.borrow().last().map(|(_, open)| *open),
        Some(false)
    );
}

#[test]
fn nothing_is_unloaded_without_an_nvidia_gpu() {
    let tree = Tree::new("no-nvidia-unload");
    let gpu = tree.device("pci0000:00/0000:03:00.0");
    tree.gpu(0, &gpu, "amdgpu");
    let fake = FakeNvidia::default();
    let start = Instant::now();
    let mut sampler = TemperatureSampler::new(tree.roots(), Some(fake.clone()), start);
    sampler.sample(start);
    sampler.set_active(false, at(start, 1_000));
    sampler.sample(at(start, 600_000));
    assert_eq!(*fake.unloads.borrow(), 0);
    assert_eq!(*fake.releases.borrow(), 0);
}
