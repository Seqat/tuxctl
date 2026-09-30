# tuxctl

`tuxctl` is a lightweight, keyboard-first control-center TUI for Linux, written
in Rust with [Ratatui](https://ratatui.rs/) and
[Crossterm](https://github.com/crossterm-rs/crossterm). One terminal window
shows the system at a glance, the processes, systemd services, the journal and
the network interfaces, and it stays cheap while it runs: about 0.6 % of one
core and under 3 MiB of memory when idle (static build; loading NVML for an
NVIDIA GPU adds about 20 MiB).

![The tuxctl Overview](screenshots/overview.png)

## What it does

- **Overview:** cards for CPU, GPU, memory, network, storage and pinned
  processes, with graphs, temperatures and power, colored by load.
- **Processes:** a sortable, searchable process list with pinning and safe
  `SIGTERM` / `SIGKILL`, confirmed before anything is sent.
- **Services:** systemd units and their states, filtered and searchable.
- **Logs:** the systemd journal, followed live, filtered by priority.
- **Network:** interfaces with live rates, addresses and counters.

Every screen works with the keyboard; the mouse can switch tabs, select rows,
sort, scroll and press buttons.

## Principles

- **Restrained:** a readable interface rather than as much as possible on
  screen.
- **Cheap while idle:** data is collected in the background at a chosen
  interval, and the screen is redrawn only when something visible changed.
- **Safe:** text from other users can never reach the terminal as escape
  sequences, and signals go only to the exact process that was confirmed.
- **Linux-native:** it reads `/proc`, `/sys`, `systemctl` and `journalctl`
  directly and asks for no privileges.

Start with [Installation](installation.md), then the [Controls](controls.md).
