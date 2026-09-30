# Command-line options

```text
tuxctl [--interval <DURATION>] [--no-nvidia-temperature] [--check]
```

| Option | Description |
| --- | --- |
| `--interval <DURATION>` | Sampling interval for CPU, memory, and network: `250ms`, `500ms`, `1s` (default), `2s`, `5s`, `10s`, `30s`, or `60s`. Processes refresh at most once per second and services at most every 5 seconds. `+` and `-` change the interval while `tuxctl` runs. |
| `--no-nvidia-temperature` | Do not load NVML for NVIDIA GPUs on the proprietary driver. NVML shows their temperature and usage but adds about 20 MiB of private memory (see [Temperatures and GPUs](hardware.md#nvidia-gpus-on-the-proprietary-driver)); static builds never load it. The Help overlay shows whether it is on. |
| `--check` | Print which sensors `tuxctl` finds on this machine, their values and origins, and what would enable the missing ones; then exit. See [Optional setup](optional-setup.md). |
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print the version. |

A `--check` report looks like this:

```text
tuxctl 0.3.3 sensor check

CPU     AMD Ryzen 5 7500F 6-Core Processor
        temperature  58°C       k10temp Tctl
        power        readable   from RAPL

GPU     NVIDIA GeForce RTX 5070 Ti  (nvidia)
        temperature  40°C       NVML
        usage        util 0%, VRAM 1.7/15.9 GiB, 27 W, fan 0%  NVML

Disk    NVMe0  WD Blue SN5100 1TB  (nvme0n1)
        temperature  41°C       nvme Composite
```
