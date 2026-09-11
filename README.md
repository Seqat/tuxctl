# tuxctl

`tuxctl` is a lightweight, responsive, Linux-native control-center TUI written in Rust with [Ratatui](https://ratatui.rs) and [Crossterm](https://github.com/crossterm-rs/crossterm). It provides real-time system monitoring, process management, systemd service status, journal logs, and network interface statistics in a restrained, terminal-native interface.

---

## Requirements

- **Operating System**: Linux (`tuxctl` directly inspects Linux interfaces such as `/proc` and `/sys`).
- **Runtime Utilities**:
  - `systemctl` (required for the **Services** tab).
  - `journalctl` (required for the **Logs** tab).
- **Rust Toolchain**: 1.74+ (Rust 2021 edition).

---

## Implemented Screens & Features

1. **Overview**
   - Live CPU, Memory, and Root Filesystem usage gauges.
   - 1-minute, 5-minute, and 15-minute system load averages and system uptime.
   - Hardware inventory:
     - CPU models and physical socket/package deduplication.
     - RAM modules via EDAC sysfs with total memory fallback from `/proc/meminfo`.
     - GPU devices (DRM and NVIDIA sysfs detection).
     - Block storage devices (NVMe, SATA/SCSI disk enumeration).
   - Responsive layout adapting from side-by-side wide displays to compact stacked layouts on narrow terminals.

2. **Processes**
   - Real-time `/proc` scanner reporting PID, process name, command, CPU percentage, and resident memory.
   - Sortable columns: CPU (`c`), Memory (`m`), PID (`p`), and Name (`n`).
   - Case-insensitive search filter (`/`).
   - Detailed process inspection modal (`Enter`).
   - Safe signal workflow:
     - Request SIGTERM (`t`) or SIGKILL (`K` / `Shift+K`).
     - Explicit confirmation modal with `Cancel` as the safe default.
     - Start-time identity validation immediately prior to signal delivery to prevent PID reuse race conditions.

3. **Services**
   - Systemd unit status viewer (`Unit`, `LoadState`, `ActiveState`, `SubState`, `Description`).
   - Distinct state glyphs (`● active`, `✖ failed`, `○ inactive`, `◌ activating`) for accessibility across color and monochrome terminals.
   - Case-insensitive search filter (`/`).
   - On-demand service refresh (`r`).
   - Read-only service inspection popup (`Enter`).

4. **Logs**
   - Streaming systemd journal reader via `journalctl`.
   - Bounded ring buffer preventing memory exhaustion under high log volume.
   - Dynamic follow mode (`f`) and pause/resume (`Space`).
   - Severity-based color coding (emergency/crit/error, warn, notice/info, debug).
   - Search/filter mode (`/`) and detailed message viewer (`Enter`).
   - Dropped entry accounting when logs exceed ingestion capacity.

5. **Network**
   - Network interface monitor parsing `/proc/net/dev` and `/sys/class/net`.
   - Dynamic RX/TX transfer rate calculations based on elapsed sampling intervals.
   - Operational link status glyphs (`● up`, `○ down`, `◌ dormant`).
   - IPv4 and IPv6 address resolution.
   - Detailed interface modal (`Enter`) with MAC address, MTU, packet counts, error counters, and drop statistics.

6. **Help**
   - Contextual keybinding reference overlay (`?`).

---

## Building and Running

### Development Mode

Run directly with Cargo:

```sh
cargo run
```

### Release Build

Build an optimized release binary:

```sh
cargo build --release
```

The resulting executable is located at:

```sh
./target/release/tuxctl
```

---

## Controls & Keybindings

### Global Controls

| Key | Action |
| --- | --- |
| `1` - `5` | Switch directly to tab (1: Overview, 2: Processes, 3: Services, 4: Logs, 5: Network) |
| `Tab` / `Shift+Tab` | Next / previous tab (also `→` / `←`) |
| `?` | Toggle Help dialog |
| `Esc` | Dismiss open dialog / clear search filter |
| `q` or `Ctrl+C` | Quit `tuxctl` |

### Navigation & Common Actions

| Key | Action |
| --- | --- |
| `↑` / `k` | Move selection up |
| `↓` / `j` | Move selection down |
| `PageUp` / `PageDown` | Move selection up / down by page |
| `Home` / `End` | Jump to first / last item |
| `/` | Begin search / filter |
| `Enter` | Open detailed inspection modal |

### Processes Screen

| Key | Action |
| --- | --- |
| `c` | Sort by CPU % (toggle descending / ascending) |
| `m` | Sort by Memory (toggle descending / ascending) |
| `p` | Sort by PID (toggle ascending / descending) |
| `n` | Sort by Name (toggle ascending / descending) |
| `t` | Request `SIGTERM` for selected process |
| `K` / `Shift+K` | Request `SIGKILL` for selected process |

#### Signal Confirmation Modal

| Key | Action |
| --- | --- |
| `Tab` / `←` / `→` / `h` / `l` | Toggle focus between `Cancel` and `Confirm` |
| `Enter` | Execute focused button |
| `Esc` | Cancel and dismiss modal |

### Services Screen

| Key | Action |
| --- | --- |
| `r` | Trigger immediate systemd service refresh |

### Logs Screen

| Key | Action |
| --- | --- |
| `f` | Toggle follow mode (auto-scroll to newest entries) |
| `Space` | Toggle pause / resume |

### Mouse Controls

- **Tab Switching**: Click any tab title in the top bar.
- **Selection**: Click any row in Processes, Services, Logs, or Network to select it.
- **Scrolling**: Scroll the mouse wheel over list areas to scroll up and down.
- **Process Sorting**: Click column headers (`PID`, `NAME`, `CPU`, `MEMORY`) to change sort column and toggle direction.
- **Dialogs**: Click `[ Cancel ]` or `[ Confirm ]` buttons in confirmation popups.

---

## Known Limitations

- **Systemd Dependency**: The Services and Logs tabs require access to `systemctl` and `journalctl` on the host system.
- **Hardware Hotplug**: Hardware inventory is discovered at startup; hot-plugged devices (such as USB drives) are not dynamically re-enumerated without restarting.
- **Process Signals**: Signal operations are subject to standard Linux permissions. Signaling processes owned by root or other users will report `permission denied` unless `tuxctl` is run with sufficient capabilities or privileges.
- **Terminal Width**: The interface adapts to small terminals, but terminal widths below 40 columns will truncate long command strings and IP addresses rather than offering horizontal scrolling.
