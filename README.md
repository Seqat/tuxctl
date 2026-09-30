# tuxctl

[![CI](https://github.com/Seqat/tuxctl/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Seqat/tuxctl/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Seqat/tuxctl)](https://github.com/Seqat/tuxctl/releases/latest)
[![crates.io](https://img.shields.io/crates/v/tuxctl)](https://crates.io/crates/tuxctl)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-blue)](Cargo.toml)

A lightweight, keyboard-first control center for the Linux terminal: system
overview, processes, systemd services, the journal and network interfaces in
one restrained TUI, written in Rust with [Ratatui](https://ratatui.rs/).

<p align="center">
  <img src="docs/screenshots/overview.png" alt="tuxctl Overview dashboard">
</p>

**[Documentation](https://seqat.github.io/tuxctl/)** ·
[Installation](https://seqat.github.io/tuxctl/installation.html) ·
[Controls](https://seqat.github.io/tuxctl/controls.html) ·
[Changelog](CHANGELOG.md)

## Features

- **Overview:** CPU, GPU, memory, network, storage and pinned-process cards
  with graphs, temperatures and power, colored by load, laid out to fit any
  terminal from 40×15 up.
- **Processes:** sort, search, pin, inspect, and send `SIGTERM` / `SIGKILL`
  only after confirmation, through pidfds so a reused PID is never hit.
- **Services:** systemd units and their states, searchable and filterable.
- **Logs:** the systemd journal, followed live, filtered by priority.
- **Network:** interfaces with live rates, addresses and error counters.
- **Cheap:** about 0.6 % of one core and under 3 MiB of memory when idle
  (static build), with redraws only when something visible changes.
- **Careful:** asks for no privileges, and text from other users can never
  reach the terminal as escape sequences.

Keyboard first, with mouse support for tabs, rows, sorting and buttons.

## Install

Static binaries for x86_64 and aarch64 Linux are attached to every
[release](https://github.com/Seqat/tuxctl/releases/latest):

```sh
arch=$(uname -m)
base=https://github.com/Seqat/tuxctl/releases/latest/download
curl -LO "$base/tuxctl-$arch-unknown-linux-musl.tar.gz" -LO "$base/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
tar xzf "tuxctl-$arch-unknown-linux-musl.tar.gz"
install -Dm755 "tuxctl-$arch-unknown-linux-musl/tuxctl" ~/.local/bin/tuxctl
```

Or from [crates.io](https://crates.io/crates/tuxctl), with Rust 1.88 or newer:

```sh
cargo install tuxctl --locked
```

The crates.io build can also show NVIDIA GPUs on the proprietary driver, which
the static binaries cannot. See
[Installation](https://seqat.github.io/tuxctl/installation.html) for
verification and other options.

## Quick start

```sh
tuxctl            # start on the Overview
tuxctl --check    # list the sensors tuxctl finds on this machine
```

| Key | Action |
| --- | --- |
| `1` – `5`, `Tab` | Switch screens |
| `↑` `↓` / `k` `j` | Move the selection |
| `/` | Search |
| `Enter` | Details |
| `T` / `K` | `SIGTERM` / `SIGKILL` the selected process (asks first) |
| `P` | Pin a process to the top and the Overview |
| `+` / `-` | Change the sampling interval |
| `?` | Help for the current screen |
| `q` | Quit (asks first); `Ctrl+C` quits at once |

All keys and mouse actions are on the
[Controls](https://seqat.github.io/tuxctl/controls.html) page.

## Requirements

Linux, with `systemctl` and `journalctl` for the Services and Logs screens.
Some sensors need an optional step, such as SATA disk temperatures or CPU
power; `tuxctl --check` tells you which, and
[Optional setup](https://seqat.github.io/tuxctl/optional-setup.html) explains
each one.

## Documentation

The [documentation](https://seqat.github.io/tuxctl/) covers each screen,
[temperatures and GPUs](https://seqat.github.io/tuxctl/hardware.html),
[design and reliability](https://seqat.github.io/tuxctl/design.html),
[performance](https://seqat.github.io/tuxctl/performance.html) and
[development](https://seqat.github.io/tuxctl/development.html). Its sources are
in [`docs/`](docs/).

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request, and the
[security policy](SECURITY.md) to report a vulnerability privately.

## License

MIT; see [LICENSE](LICENSE).
