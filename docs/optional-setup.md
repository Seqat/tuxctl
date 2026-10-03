# Optional setup

Everything works without setup. Three things need a step on some machines, and
`tuxctl --check` shows which apply to yours: it lists the sensors `tuxctl`
finds, with their values and where they come from, and says what would enable
the missing ones.

| To see | You need |
| --- | --- |
| SATA / SAS disk temperatures | the `drivetemp` kernel module ([below](#sata-and-sas-disk-temperatures)) |
| CPU package power | readable RAPL energy counters ([below](#cpu-power)) |
| Temperature and usage of NVIDIA GPUs on the proprietary driver | the glibc release binary or a source build of `tuxctl` ([below](#nvidia-gpus)) |

`tuxctl` itself never changes the system and asks for no privileges; each step
below is one you take yourself, and each can be undone.

## SATA and SAS disk temperatures

These need the kernel's `drivetemp` module, which ships with the kernel but is
usually not loaded. To load it now and at every boot:

```sh
sudo modprobe drivetemp
echo drivetemp | sudo tee /etc/modules-load.d/drivetemp.conf
```

Restart `tuxctl` afterwards: it looks for sensors at startup. To undo, delete
`/etc/modules-load.d/drivetemp.conf`.

> **Hard disks:** per the
> [kernel documentation](https://docs.kernel.org/hwmon/drivetemp.html),
> reading the temperature may reset the spin-down timer on some drives
> (observed with WD120EFAX). `tuxctl` reads it every 2 seconds, so such a
> drive would never spin down. SSDs do not spin, so this does not concern
> them. If you rely on hard disks spinning down, leave `drivetemp` unloaded.

## CPU power

Intel and AMD CPUs report their package energy through RAPL
(`/sys/class/powercap/intel-rapl:N/energy_uj`), but since Linux 5.10 only root
can read it: unprivileged access allowed a side-channel attack on the CPU
(PLATYPUS, CVE-2020-8694). `tuxctl` shows the package power when the counter is
readable, computed from the energy used between two readings 2 seconds apart,
and nothing otherwise. The out-of-tree `zenpower` driver (AMD Zen 1–3) reports
power directly and needs no step.

To make only the energy counters readable, now and at every boot:

```sh
echo 'ACTION=="add", SUBSYSTEM=="powercap", KERNEL=="intel-rapl:[0-9]*", RUN+="/usr/bin/chmod a+r /sys%p/energy_uj"' | sudo tee /etc/udev/rules.d/90-rapl-energy.rules
sudo udevadm trigger --subsystem-match=powercap --action=add
```

Restart `tuxctl` afterwards. This lets every local user read the energy
counters again, and so reopens that side channel; weigh it on a shared
machine. To undo, delete the rule and reboot.

Some monitors are instead installed with the `cap_dac_read_search`
capability, which lets them read every file on the system regardless of its
permissions; `tuxctl` does not need or recommend that.

## NVIDIA GPUs

GPUs on NVIDIA's proprietary driver report through NVML, which `tuxctl` loads
at run time. The static release binaries cannot load it; use a glibc build
instead. On x86_64 that can be the release's
`tuxctl-x86_64-unknown-linux-gnu.tar.gz` (glibc 2.28 or newer; see
[Installation](installation.md#nvidia-gpus-the-glibc-binary)); anywhere, a
build from source:

```sh
cargo install tuxctl --locked
```

NVML adds about 20 MiB of memory and some CPU time; see
[NVIDIA GPUs on the proprietary driver](hardware.md#nvidia-gpus-on-the-proprietary-driver)
for the figures, and `--no-nvidia-temperature` to leave it unloaded.
