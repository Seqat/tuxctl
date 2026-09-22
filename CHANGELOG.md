# Changelog

All notable changes to `tuxctl` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.5] - 2026-09-23

Groundwork for v0.3.0, plus a few small features pulled forward.

### Added

- `--interval <DURATION>` sets the sampling interval for CPU, memory and
  network (`250ms` to `60s`; default `1s`). Process scans stay at most once
  per second and `systemctl` at most every 5 seconds.
- `-h`/`--help` and `-V`/`--version`.
- The Overview CPU history states the time span its sparkline covers.
- The process detail view shows whether a process is a kernel thread.
- Journal timestamps are shown in local time; the log detail view includes
  the full date and UTC offset.
- CI on GitHub Actions (fmt, clippy, tests, MSRV 1.88) and development
  scripts for pty smoke tests and CPU/RSS/redraw measurements
  (`scripts/`, opt-in `redraw-counter` feature).

### Changed

- The process sort indicator is shown at every terminal width.
- Stale markers use each collector's own sampling period.

### Fixed

- Status lines no longer hide an active search or filter: on Processes a
  signal result or refresh error, and on Services "Refreshing…" or a
  refresh error, now appear after it instead of replacing it.
- A timed-out `systemctl` that had spawned helper processes could keep its
  output pipes open and delay collector shutdown; the whole process group is
  now killed.

### Internal

- `src/app.rs` split into per-screen modules; modals tracked as a single
  overlay; global tab keys shared; key-binding, README and Esc-order tests;
  shared counter-rate helper.

## [0.2.2] - 2026-09-22

Collector lifecycle and narrow-layout release. No new keys or screens.

### Changed

- `journalctl` starts on the first visit to the Logs tab instead of at
  startup.
- Services are collected only while the Services tab is visible; opening the
  tab requests one immediate refresh. A hidden, paused collector is not
  reported as stale.
- All background collectors share one stop/pause/refresh control primitive.

### Fixed

- Overview no longer drops logical CPUs without an overflow line, no longer
  shows fewer CPUs in a taller terminal, and no longer renders RAM, GPU,
  storage, or network as a heading without content at narrow widths.
- Dense CPU cells keep a space between the label and the value.
- Tab labels no longer run together at 40 columns; narrow terminals use short
  labels (`Ovr Proc Svc Logs Net`) with matching mouse targets.

## [0.2.1] - 2026-09-22

Reliability and efficiency release. No new keys or screens.

### Added

- A `stale` marker in the frame title when the metrics, process, network, or
  services collector behind the current screen stops delivering data.
- `rust-version = "1.88"` in `Cargo.toml` (the minimum supported Rust version
  for the locked dependencies).

### Changed

- The main loop applies all ready updates before rendering, so snapshots that
  arrive together cost one redraw. Background data redraws at most every
  50 ms; keyboard, mouse, and resize still redraw immediately.
- Process command lines are cached per `(PID, start_time)` instead of being
  re-read from `/proc` every second.
- Process sort and search keys are lowercased once per snapshot, and hidden
  Processes snapshots defer filtering and sorting until the tab is shown.

### Fixed

- A hung `systemctl` can no longer stall the Services tab or block exit. The
  listing is bounded by a 10 s timeout and reports `systemctl timed out`.
- The Logs `PRIORITY` header is no longer truncated.
- The Overview root filesystem row no longer draws its label over the gauge.
- Interfaces reporting an `unknown` state (such as `lo`) are shown neutrally
  as `◌ unknown` instead of an uppercase warning.

## [0.2.0] - 2026-09-17

### Added

- Redesigned Overview with side-by-side or stacked System and Hardware
  panels: CPU history, load averages, per-CPU grid, RAM usage, and hardware
  inventory (CPU model, RAM modules, GPUs, NVMe/SATA storage).
- Compact network interface summary with live RX/TX rates on Overview.
- Aggregate CPU and RAM summary on the Processes screen.
- Minimum terminal size (40x15) with an explicit warning below it.

### Changed

- Collector communication is bounded and snapshot-based.
- Journal ingestion is bounded and scheduled so sustained log traffic cannot
  starve terminal input, quit, or resize handling.
- Search filtering is scoped to the active tab.

### Fixed

- Process signals are delivered through pidfds after identity revalidation,
  closing PID-reuse races; signaling fails closed when pidfds are unavailable.
- Resize handling and terminal teardown ordering were hardened.
- Network rates reset safely after `/proc/net/dev` read failures.

## [0.1.0] - 2026-09-11

- Initial release with Overview, Processes, Services, Logs, and Network screens.

[Unreleased]: https://github.com/Seqat/tuxctl/compare/v0.2.5...HEAD
[0.2.5]: https://github.com/Seqat/tuxctl/compare/v0.2.2...v0.2.5
[0.2.2]: https://github.com/Seqat/tuxctl/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/Seqat/tuxctl/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Seqat/tuxctl/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Seqat/tuxctl/releases/tag/v0.1.0
