# Design and reliability

`tuxctl` is built around a small, bounded, event-driven flow:

```text
/proc, /sys, systemctl, journalctl
        │   background collectors
        ▼
bounded snapshots (latest value only)
        │
        ▼
App state ◀── keyboard and mouse actions
        │
        ▼
render from cached state ──▶ hit regions for the mouse
```

## Staying cheap

- Linux data is collected on background threads; drawing uses only cached
  state and never reads `/proc` or `/sys` or waits on `systemctl` or
  `journalctl`.
- Snapshots that arrive together cost one redraw, and background redraws are
  limited to one every 50 ms; keyboard, mouse and resize still redraw at once.
- Mouse movement redraws only when the element under the pointer changes.
- Screens that are not visible update their data without redrawing.
- `systemctl` and `journalctl` start only when their screens are first opened;
  services are collected only while their screen is visible, and hardware
  sensors only while the Overview is.
- Histories, the log buffer and every queue between threads have a fixed size.

## Staying correct

- If a collector stops delivering data, the frame title shows a `stale` marker
  for that screen instead of presenting frozen data as live.
- A `systemctl` call is stopped after 10 seconds, so it cannot stall the
  Services screen or exit.
- An active search or filter stays visible in the status line; messages appear
  after it, never instead of it.
- `SIGTERM`, `SIGHUP` and `SIGINT` quit through the same path as `q`, so the
  terminal is always restored. A crash in a background thread does not touch
  the terminal.

## Security

- **Untrusted text:** process names, command lines, journal messages, unit
  descriptions and other system data can be set by other users. Control
  characters and bidirectional overrides are removed before anything reaches
  the terminal, so escape sequences planted in them cannot act on it.
- **Bounded input:** command lines are read up to 4 KiB per process, and
  journal fields over 4 KiB arrive empty from `journalctl`, so other users
  cannot make `tuxctl` hold large amounts of memory.
- **Signals:** a signal goes through a pidfd to the process whose
  `(PID, start time)` was confirmed; a process that exited or a new one that
  reused the PID is never signaled, and without pidfds nothing is sent.
- **No privileges:** `tuxctl` asks for none and changes nothing on the system;
  features that need root-only data are simply not shown.
- **Code:** every `unsafe` block states why it is sound, and code outside
  tests may not panic on errors (both enforced by `clippy`). Dependencies are
  checked against RustSec advisories on every push and weekly.

To report a vulnerability, see the
[security policy](https://github.com/Seqat/tuxctl/blob/main/SECURITY.md).

## Testing

Every push and pull request runs the unit and command-line tests, `clippy`,
the minimum supported Rust version, a pseudo-terminal smoke test of the release
binary, and guards on redraw rates and memory. See [Development](development.md).
