# Temperatures and GPUs

## Temperatures

A temperature appears next to a component only when the kernel provides a
sensor for it; components without one show nothing. `–` means the sensor
exists but has no value right now, for example while a GPU is
runtime-suspended. Sensors are read at most every 2 seconds, whatever the
sampling interval, and only while the Overview is visible.

| Component | Source |
| --- | --- |
| Intel CPU | `coretemp`: the `Package id N` sensor, or the hottest core when there is none |
| AMD CPU | `k10temp` or `zenpower`: `Tdie`, else `Tctl` (per-CCD sensors are not used) |
| Other CPUs (ARM, SoCs) | Only without a CPU hwmon driver: the hottest thermal zone whose type names the CPU or SoC (never `acpitz`) |
| AMD GPU | `amdgpu` (the `edge` sensor) or `radeon` hwmon |
| Intel GPU | `i915` / `xe` hwmon, when the GPU has its own sensor; integrated GPUs usually do not |
| NVIDIA GPU, `nouveau` | `nouveau` hwmon |
| NVIDIA GPU, proprietary driver | NVML (`libnvidia-ml.so.1`, installed with the driver); not in the static release binaries |
| NVMe | `nvme` hwmon (`Composite`) |
| SATA / SAS | `drivetemp` hwmon, only when that module is loaded (see [Optional setup](optional-setup.md#sata-and-sas-disk-temperatures)) |
| Network adapter | A hwmon sensor on the adapter or on its PHY |

RAM (SPD) sensors are not shown.

### Colors

A temperature takes the color band (see [Overview](overview.md#colors)) of its
share of the component's critical temperature: the limit the driver reports
(`temp*_crit`, else `temp*_max`; for NVIDIA GPUs the slowdown temperature from
NVML), or, when it reports none, an assumed limit:

| Component | Assumed critical temperature |
| --- | --- |
| CPU | 95 °C |
| GPU | 95 °C |
| NVMe | 80 °C |
| SATA / SAS and other disks | 60 °C |
| Network adapter | 100 °C |

The value is always shown, so the color never carries meaning alone.

## GPU usage

| Driver | Utilization | VRAM | Power | Fan |
| --- | --- | --- | --- | --- |
| NVIDIA proprietary | NVML | NVML | NVML | NVML |
| `amdgpu` | `gpu_busy_percent` | `mem_info_vram_*` | hwmon | hwmon |
| `nouveau` | – | – | hwmon | hwmon |
| Intel (`i915`, `xe`) | – | – | – | – |

A part the driver does not report is left out rather than shown as a
placeholder.

## NVIDIA GPUs on the proprietary driver

NVIDIA's proprietary driver has no hwmon sensors, so `tuxctl` reads these GPUs
through NVML, the library the driver installs (`libnvidia-ml.so.1`). It loads
NVML at run time, and only when it finds a GPU using the `nvidia` driver.

- **Memory:** NVML adds about 20 MiB of private memory and a thread from the
  first reading on (on the reference machine: `RssAnon` +20.2 MiB, PSS
  +21.4 MiB; RSS +24.7 MiB including 4.5 MiB of shared library pages). The
  library keeps it until `tuxctl` exits. Other monitors that read NVIDIA GPUs
  load the same library and pay the same cost.
- **CPU:** utilization and power are read on every sample, temperature, VRAM
  and fan every 2 seconds. That adds about 0.35 % of one core at the default
  1 s interval and about 0.65 % at 250 ms on the reference machine, and only
  while the Overview is visible.
- **Opting out:** `--no-nvidia-temperature` leaves NVML unloaded.
- **Static binaries:** the static release binaries cannot load NVML at all, so
  they show nothing for these GPUs. Use a glibc build, such as
  `cargo install tuxctl --locked`.

## Sleeping GPUs

`tuxctl` never wakes a sleeping GPU: it reads `power/runtime_status` first and
shows `–` while the GPU is suspended.

NVML stays initialized only when the GPU cannot runtime-suspend anyway
(`power/control` is `on`, or the driver reports `Runtime D3 status` as not
supported or disabled). With RTD3 enabled, as on many hybrid laptops, NVML is
initialized for each reading and shut down right after, and only while every
NVIDIA GPU is awake, so `tuxctl` never keeps the GPU powered.
