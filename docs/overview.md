# Overview

The first screen: the whole system at a glance, as a set of cards that uses the
full terminal.

![The Overview](screenshots/overview.png)

## Summary line

The top border shows the hostname, kernel, uptime, and the process, running
and zombie counts (zombies are highlighted when there are any). On a narrow
terminal the kernel goes first, then the counts shorten to `397p · 2r · 0z`,
then the uptime goes.

## Cards

Each card names its component in the title, with the model, temperature and
power where known: `GPU  RTX 5070 Ti · 43°C · 28W`. When the title does not
fit, the model is shortened first.

- **CPU:** a graph of total utilization, utilization and the 1/5/15-minute load
  averages, and a grid of every logical CPU. Package power appears where this
  user can read it (see [CPU power](optional-setup.md#cpu-power)).
- **GPU:** the discrete GPU (or the only one) with a utilization graph,
  utilization, VRAM use and fan speed where the driver reports them, and a row
  for each other GPU. See [Temperatures and GPUs](hardware.md).
- **Memory:** a graph of RAM use, RAM and swap gauges, and the RAM modules when
  EDAC reports them.
- **Network:** the main physical interface with its state and temperature, a
  graph of its traffic with the peak, and a row for each other interface. The
  graph is logarithmic, so one spike does not flatten everyday traffic;
  traffic below 1 KiB/s stays at the baseline.
- **Storage:** usage of every local filesystem (one line per device, so btrfs
  subvolumes appear once; network, FUSE and loop mounts are left out), and the
  NVMe, SATA and SCSI disks with their temperature and read/write throughput.
- **Pinned:** the processes pinned with `P` on the Processes screen, with their
  CPU and memory. A pinned process that exits stays for a few seconds, dimmed,
  as `exited`.

Graphs keep the last 240 samples and show as many as fit the card; the bottom
border states the time span shown. They start over when the sampling interval
changes.

Temperatures, GPU usage, CPU power and filesystem use are read only while the
Overview is visible; the other screens keep the last values.

## Colors

Utilization values (total and per-CPU, RAM, GPU utilization and VRAM), pinned
processes' CPU, and each column of the CPU, memory and GPU graphs take a band:

| Value | Color |
| --- | --- |
| below 10 % | light blue |
| 10 % to 65 % | green |
| 65 % to 80 % | yellow |
| 80 % to 95 % | orange |
| 95 % and above | red |

A process busy on several cores counts as 100 %. The network graph shows
throughput rather than a percentage and stays neutral. Temperatures use the
same bands as a share of their critical limit (see
[Temperatures and GPUs](hardware.md)). A value is always shown as text, so the
color never carries meaning alone.

`tuxctl` reads `COLORTERM` and `TERM` once at startup: `truecolor` or `24bit`
terminals get the full palette, `*256color` terminals the nearest 256-color
entries, and anything else the 16 basic colors (where orange becomes bright
red).

## Layout

The cards arrange themselves by terminal width:

- **150 columns and more:** a 2×2 grid (CPU | GPU, Memory | Network), Storage
  below it, and Pinned as a column on the right.
- **100 to 149 columns:** the same grid, with Storage and Pinned side by side
  below it.
- **Narrower:** the cards stacked in priority order (CPU, Memory, Pinned,
  Network, Storage, GPU), each graph in its card's title row, or above the
  card's rows when the terminal is tall enough for every card that way.

Cards side by side are equally wide, so their graphs span the same time. A
short terminal gives up, in this order: the optional rows (the per-CPU grid,
other GPUs and interfaces, memory modules), then graph height down to one row,
then list rows (Pinned and Storage then say how many they leave out), and only
then whole cards.
