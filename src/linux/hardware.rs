use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
};

const CPUINFO: &str = "/proc/cpuinfo";
const MEMINFO: &str = "/proc/meminfo";
const EDAC_ROOT: &str = "/sys/devices/system/edac/mc";
const DRM_ROOT: &str = "/sys/class/drm";
const NVIDIA_ROOT: &str = "/proc/driver/nvidia/gpus";
const BLOCK_ROOT: &str = "/sys/block";
const NETWORK_ROOT: &str = "/sys/class/net";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HardwareInventory {
    pub cpus: Vec<CpuPackage>,
    pub memory_modules: Vec<MemoryModule>,
    pub total_memory: Option<u64>,
    pub gpus: Vec<GpuDevice>,
    pub storage_devices: Vec<StorageDevice>,
    pub network_devices: Vec<NetworkDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuPackage {
    pub physical_id: Option<u32>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryModule {
    pub locator: Option<String>,
    pub capacity_bytes: u64,
    pub memory_type: Option<String>,
    pub speed_mts: Option<u64>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuKind {
    Integrated,
    Discrete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDevice {
    pub model: String,
    pub kind: Option<GpuKind>,
    pub vram_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Nvme,
    Sata,
    Scsi,
    Virtio,
    Mmc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDevice {
    pub system_name: String,
    pub kind: StorageKind,
    pub model: Option<String>,
    pub capacity_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkDevice {
    pub interface_name: String,
    pub model: Option<String>,
}

pub struct HardwareCollector {
    receiver: Receiver<HardwareInventory>,
}

impl HardwareCollector {
    pub fn start() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("hardware-discovery".into())
            .spawn(move || {
                let _ = sender.send(discover());
            })?;

        Ok(Self { receiver })
    }

    pub fn latest(&self) -> Option<HardwareInventory> {
        self.receiver.try_iter().last()
    }
}

fn discover() -> HardwareInventory {
    HardwareInventory {
        cpus: fs::read_to_string(CPUINFO)
            .ok()
            .map(|contents| parse_cpu_packages(&contents))
            .unwrap_or_default(),
        memory_modules: discover_memory_modules(Path::new(EDAC_ROOT)),
        total_memory: fs::read_to_string(MEMINFO)
            .ok()
            .and_then(|contents| parse_total_memory(&contents)),
        gpus: discover_gpus(Path::new(DRM_ROOT), Path::new(NVIDIA_ROOT)),
        storage_devices: discover_storage(Path::new(BLOCK_ROOT)),
        network_devices: discover_network_devices(Path::new(NETWORK_ROOT)),
    }
}

fn discover_network_devices(root: &Path) -> Vec<NetworkDevice> {
    read_sorted_directories(root, |_| true)
        .into_iter()
        .filter_map(|interface| {
            let interface_name = interface.file_name()?.to_str()?.to_owned();
            let device = fs::canonicalize(interface.join("device")).ok()?;
            Some(NetworkDevice {
                interface_name,
                model: discover_nic_model(&device),
            })
        })
        .collect()
}

fn discover_nic_model(device: &Path) -> Option<String> {
    let model = join_nonempty([
        read_trimmed(device.join("manufacturer")),
        read_trimmed(device.join("product")),
    ])
    .or_else(|| read_trimmed(device.join("product_name")))
    .or_else(|| read_trimmed(device.join("model")));
    if model.is_some() {
        return model;
    }

    let is_usb_interface = device
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(':'));
    is_usb_interface
        .then(|| device.parent())
        .flatten()
        .and_then(|parent| {
            join_nonempty([
                read_trimmed(parent.join("manufacturer")),
                read_trimmed(parent.join("product")),
            ])
            .or_else(|| read_trimmed(parent.join("product_name")))
            .or_else(|| read_trimmed(parent.join("model")))
        })
}

#[derive(Default)]
struct CpuRecord {
    physical_id: Option<u32>,
    model: Option<String>,
}

fn parse_cpu_packages(contents: &str) -> Vec<CpuPackage> {
    let records = contents
        .split("\n\n")
        .filter_map(|section| {
            let mut record = CpuRecord::default();
            for line in section.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let key = key.trim();
                let value = value.trim();
                match key {
                    "physical id" => record.physical_id = value.parse().ok(),
                    "model name" | "Hardware" if !value.is_empty() => {
                        record.model = Some(value.to_owned());
                    }
                    "Processor" if record.model.is_none() && !value.is_empty() => {
                        record.model = Some(value.to_owned());
                    }
                    _ => {}
                }
            }
            record.model.as_ref()?;
            Some(record)
        })
        .collect::<Vec<_>>();

    let with_physical_ids = records.iter().any(|record| record.physical_id.is_some());
    if with_physical_ids {
        let mut packages = BTreeMap::new();
        for record in records {
            if let (Some(physical_id), Some(model)) = (record.physical_id, record.model) {
                packages.entry(physical_id).or_insert(model);
            }
        }
        return packages
            .into_iter()
            .map(|(physical_id, model)| CpuPackage {
                physical_id: Some(physical_id),
                model,
            })
            .collect();
    }

    let mut seen = HashSet::new();
    records
        .into_iter()
        .filter_map(|record| record.model)
        .filter(|model| seen.insert(model.clone()))
        .map(|model| CpuPackage {
            physical_id: None,
            model,
        })
        .collect()
}

fn parse_total_memory(contents: &str) -> Option<u64> {
    let kib = contents.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == "MemTotal").then(|| value.split_whitespace().next()?.parse::<u64>().ok())?
    })?;
    kib.checked_mul(1024)
}

fn discover_memory_modules(root: &Path) -> Vec<MemoryModule> {
    let mut paths = read_sorted_directories(root, |name| name.starts_with("mc"));
    let mut modules = Vec::new();

    for controller in paths.drain(..) {
        for path in read_sorted_directories(&controller, |name| name.starts_with("dimm")) {
            let Some(size) = read_trimmed(path.join("size")) else {
                continue;
            };
            let module = parse_edac_memory_module(
                &size,
                read_trimmed(path.join("dimm_label"))
                    .or_else(|| read_trimmed(path.join("dimm_location"))),
                read_trimmed(path.join("dimm_mem_type")),
                read_trimmed(path.join("dimm_speed")).or_else(|| read_trimmed(path.join("speed"))),
            );
            if let Some(module) = module {
                modules.push(module);
            }
        }
    }

    modules
}

fn parse_edac_memory_module(
    size_mib: &str,
    locator: Option<String>,
    memory_type: Option<String>,
    speed_mts: Option<String>,
) -> Option<MemoryModule> {
    let size_mib = size_mib.trim().parse::<u64>().ok()?;
    if size_mib == 0 {
        return None;
    }

    Some(MemoryModule {
        locator: meaningful(locator),
        capacity_bytes: size_mib.checked_mul(1024 * 1024)?,
        memory_type: meaningful(memory_type),
        speed_mts: speed_mts.and_then(|speed| first_number(&speed)),
        manufacturer: None,
        part_number: None,
    })
}

fn discover_gpus(drm_root: &Path, nvidia_root: &Path) -> Vec<GpuDevice> {
    let names = fs::read_dir(drm_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect::<Vec<_>>();

    sorted_drm_cards(names)
        .into_iter()
        .filter_map(|(_, name)| read_gpu(&drm_root.join(name), nvidia_root))
        .collect()
}

fn sorted_drm_cards(names: impl IntoIterator<Item = String>) -> Vec<(u32, String)> {
    let mut cards = names
        .into_iter()
        .filter_map(|name| drm_card_index(&name).map(|index| (index, name)))
        .collect::<Vec<_>>();
    cards.sort_by_key(|(index, _)| *index);
    cards.dedup_by_key(|(index, _)| *index);
    cards
}

fn drm_card_index(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix("card")?;
    (!suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit()))
        .then(|| suffix.parse().ok())?
}

fn read_gpu(card: &Path, nvidia_root: &Path) -> Option<GpuDevice> {
    let device = card.join("device");
    let vendor = read_hex(device.join("vendor"))?;
    let device_id = read_hex(device.join("device"));
    let pci_address = fs::canonicalize(&device)
        .ok()
        .and_then(|path| path.file_name()?.to_str().map(str::to_owned));
    let nvidia = pci_address
        .as_deref()
        .and_then(|address| fs::read_to_string(nvidia_root.join(address).join("information")).ok())
        .map(|contents| parse_nvidia_information(&contents));

    let model = nvidia
        .as_ref()
        .and_then(|information| information.model.clone())
        .or_else(|| read_trimmed(device.join("product_name")))
        .or_else(|| read_trimmed(device.join("name")))
        .unwrap_or_else(|| fallback_gpu_name(vendor, device_id));
    let vram_bytes = read_trimmed(device.join("mem_info_vram_total"))
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .or_else(|| nvidia.and_then(|information| information.vram_bytes));
    let kind = classify_gpu(&device);

    Some(GpuDevice {
        model,
        kind,
        vram_bytes,
    })
}

fn classify_gpu(device: &Path) -> Option<GpuKind> {
    let explicit = read_trimmed(device.join("is_integrated"))
        .or_else(|| read_trimmed(device.join("integrated")))
        .and_then(|value| match value.to_ascii_lowercase().as_str() {
            "1" | "y" | "yes" | "true" => Some(GpuKind::Integrated),
            "0" | "n" | "no" | "false" => Some(GpuKind::Discrete),
            _ => None,
        });
    explicit.or_else(|| {
        read_trimmed(device.join("current_link_width"))
            .and_then(|width| first_number(&width))
            .filter(|width| *width > 0)
            .map(|_| GpuKind::Discrete)
    })
}

fn fallback_gpu_name(vendor: u32, device: Option<u32>) -> String {
    let vendor = match vendor {
        0x1002 => "AMD",
        0x10de => "NVIDIA",
        0x8086 => "Intel",
        _ => "GPU",
    };
    device.map_or_else(
        || format!("{vendor} graphics device"),
        |device| format!("{vendor} graphics device [{device:04x}]"),
    )
}

#[derive(Default)]
struct NvidiaInformation {
    model: Option<String>,
    vram_bytes: Option<u64>,
}

fn parse_nvidia_information(contents: &str) -> NvidiaInformation {
    let mut information = NvidiaInformation::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "Model" => information.model = meaningful(Some(value.trim().to_owned())),
            "Video Memory" => {
                information.vram_bytes =
                    first_number(value).and_then(|mib| mib.checked_mul(1024 * 1024));
            }
            _ => {}
        }
    }
    information
}

fn discover_storage(root: &Path) -> Vec<StorageDevice> {
    let mut devices = fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let path = entry.path();
            let vendor = read_trimmed(path.join("device/vendor"));
            let canonical = fs::canonicalize(&path).ok();
            let kind = storage_kind(&name, vendor.as_deref(), canonical.as_deref())?;
            let display_vendor = vendor.filter(|vendor| vendor.trim() != "ATA");
            let model = join_nonempty([
                display_vendor,
                read_trimmed(path.join("device/model"))
                    .or_else(|| read_trimmed(path.join("device/name"))),
            ]);
            let capacity_bytes = read_trimmed(path.join("size"))
                .and_then(|sectors| sectors.parse::<u64>().ok())
                .and_then(|sectors| sectors.checked_mul(512))
                .filter(|bytes| *bytes > 0);

            Some(StorageDevice {
                system_name: name,
                kind,
                model,
                capacity_bytes,
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| left.system_name.cmp(&right.system_name));
    devices
}

fn storage_kind(name: &str, vendor: Option<&str>, canonical: Option<&Path>) -> Option<StorageKind> {
    if is_nvme_disk(name) {
        return Some(StorageKind::Nvme);
    }
    if is_letters_after(name, "sd") {
        let is_sata = vendor.is_some_and(|vendor| vendor.trim() == "ATA")
            || canonical.is_some_and(|path| path.to_string_lossy().contains("/ata"));
        return Some(if is_sata {
            StorageKind::Sata
        } else {
            StorageKind::Scsi
        });
    }
    if is_letters_after(name, "vd") || is_letters_after(name, "xvd") {
        return Some(StorageKind::Virtio);
    }
    if name
        .strip_prefix("mmcblk")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(StorageKind::Mmc);
    }
    None
}

fn is_nvme_disk(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("nvme") else {
        return false;
    };
    let Some((controller, namespace)) = rest.split_once('n') else {
        return false;
    };
    !controller.is_empty()
        && controller.chars().all(|c| c.is_ascii_digit())
        && !namespace.is_empty()
        && namespace.chars().all(|c| c.is_ascii_digit())
}

fn is_letters_after(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_alphabetic()))
}

fn read_sorted_directories(root: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            keep(name.to_str()?).then_some(entry.path())
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| meaningful(Some(value)))
}

fn meaningful(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_owned()).filter(|value| {
        !value.is_empty()
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "unknown" | "not specified"
            )
    })
}

fn read_hex(path: impl AsRef<Path>) -> Option<u32> {
    let value = read_trimmed(path)?;
    u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

fn first_number(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .find_map(|part| part.parse::<u64>().ok())
}

fn join_nonempty(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let value = values.into_iter().flatten().collect::<Vec<_>>().join(" ");
    meaningful(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tuxctl-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn deduplicates_logical_cpus_by_physical_package() {
        let cpuinfo = "processor: 0\nphysical id: 1\nmodel name: Example CPU\n\nprocessor: 1\nphysical id: 1\nmodel name: Example CPU\n\nprocessor: 2\nphysical id: 3\nmodel name: Other CPU\n";

        assert_eq!(
            parse_cpu_packages(cpuinfo),
            vec![
                CpuPackage {
                    physical_id: Some(1),
                    model: "Example CPU".into(),
                },
                CpuPackage {
                    physical_id: Some(3),
                    model: "Other CPU".into(),
                },
            ]
        );
    }

    #[test]
    fn deduplicates_cpu_models_when_package_ids_are_missing() {
        let cpuinfo =
            "processor: 0\nmodel name: Virtual CPU\n\nprocessor: 1\nmodel name: Virtual CPU\n";

        assert_eq!(parse_cpu_packages(cpuinfo).len(), 1);
    }

    #[test]
    fn parses_edac_ram_module_and_ignores_empty_slots() {
        let module = parse_edac_memory_module(
            "16384\n",
            Some("DIMM_A1\n".into()),
            Some("DDR5\n".into()),
            Some("6000 MT/s\n".into()),
        )
        .unwrap();

        assert_eq!(module.capacity_bytes, 16 * 1024 * 1024 * 1024);
        assert_eq!(module.locator.as_deref(), Some("DIMM_A1"));
        assert_eq!(module.memory_type.as_deref(), Some("DDR5"));
        assert_eq!(module.speed_mts, Some(6000));
        assert!(parse_edac_memory_module("0", None, None, None).is_none());
    }

    #[test]
    fn ram_gracefully_falls_back_to_total_only() {
        let inventory = HardwareInventory {
            total_memory: parse_total_memory("MemTotal: 33554432 kB\n"),
            ..HardwareInventory::default()
        };

        assert!(inventory.memory_modules.is_empty());
        assert_eq!(inventory.total_memory, Some(32 * 1024 * 1024 * 1024));
    }

    #[test]
    fn nic_model_uses_only_available_hardware_identity() {
        let root = temp_test_dir("nic-model");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("manufacturer"), "Realtek\n").unwrap();
        fs::write(root.join("product"), "RTL8125 2.5GbE\n").unwrap();

        assert_eq!(
            discover_nic_model(&root).as_deref(),
            Some("Realtek RTL8125 2.5GbE")
        );
        fs::remove_file(root.join("manufacturer")).unwrap();
        fs::remove_file(root.join("product")).unwrap();
        assert_eq!(discover_nic_model(&root), None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn physical_nic_without_model_is_retained() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("physical-nic");
        let devices = root.join("devices");
        let interfaces = root.join("net");
        fs::create_dir_all(&devices).unwrap();
        fs::create_dir_all(interfaces.join("enp6s0")).unwrap();
        symlink(&devices, interfaces.join("enp6s0/device")).unwrap();

        let discovered = discover_network_devices(&interfaces);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].interface_name, "enp6s0");
        assert_eq!(discovered[0].model, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn enumerates_multiple_drm_cards_without_connector_duplicates() {
        let cards = sorted_drm_cards([
            "card2".into(),
            "card0-HDMI-A-1".into(),
            "renderD128".into(),
            "card0".into(),
            "card1".into(),
        ]);

        assert_eq!(
            cards,
            vec![
                (0, "card0".into()),
                (1, "card1".into()),
                (2, "card2".into())
            ]
        );
    }

    #[test]
    fn missing_gpu_metadata_uses_ids_without_inventing_vram_or_kind() {
        let gpu = GpuDevice {
            model: fallback_gpu_name(0x8086, Some(0x1234)),
            kind: None,
            vram_bytes: None,
        };

        assert_eq!(gpu.model, "Intel graphics device [1234]");
        assert_eq!(gpu.kind, None);
        assert_eq!(gpu.vram_bytes, None);
    }

    #[test]
    fn parses_nvidia_model_and_vram() {
        let information = parse_nvidia_information(
            "Model: NVIDIA GeForce Example\nVideo Memory: 16384 MB\nIRQ: 42\n",
        );

        assert_eq!(information.model.as_deref(), Some("NVIDIA GeForce Example"));
        assert_eq!(information.vram_bytes, Some(16 * 1024 * 1024 * 1024));
    }

    #[test]
    fn filters_synthetic_storage_and_classifies_supported_devices() {
        assert_eq!(storage_kind("nvme0n1", None, None), Some(StorageKind::Nvme));
        assert_eq!(
            storage_kind("sda", Some("ATA"), None),
            Some(StorageKind::Sata)
        );
        assert_eq!(
            storage_kind("sdb", Some("IBM"), None),
            Some(StorageKind::Scsi)
        );
        assert_eq!(storage_kind("vda", None, None), Some(StorageKind::Virtio));
        assert_eq!(storage_kind("mmcblk0", None, None), Some(StorageKind::Mmc));
        for name in ["loop0", "ram0", "zram0", "dm-0", "md0", "nvme0n1p1"] {
            assert_eq!(storage_kind(name, None, None), None);
        }
    }
}
