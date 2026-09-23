# Development scripts

Python 3 (standard library only) helpers that drive a release build of `tuxctl` in a
pseudo-terminal. They need Linux with systemd. CI runs `smoke.py` and a short
`measure.py` on every push and pull request (see `.github/workflows/`).

| Script | Purpose |
| --- | --- |
| `smoke.py BINARY` | Runs the CLAUDE.md smoke list: tabs, mouse, resize down to 1×1, navigation, sorting, search, details, SIGKILL confirmation, Services, Logs, Help, quit and terminal restore, plus collector processes (`journalctl` only after the Logs tab is visited, nothing left in the session after exit) and SIGTERM/SIGHUP shutdown. It exits with a non-zero status if any check fails. |
| `measure.py BINARY [--label NAME] [--secs N] [--json PATH] [--check]` | Prints CPU %, RSS and redraws/s for idle Overview, Processes and Logs, a log storm and rapid hover, then time to first frame, input latency during a storm and RSS growth once the log ring is full. |
| `rss.py BINARY MINUTES [--every SECS] [--json PATH] [--max-growth KIB --after MIN]` | Prints RSS once a minute (or every `SECS`) while tuxctl idles on Overview; with `--max-growth` it exits non-zero if RSS grew by more than `KIB` after minute `MIN` (default 5). |

`measure.py` counts redraws only when the binary is built with the development
feature. Without it, the redraws/s column is 0:

```sh
cargo build --release --features redraw-counter --target-dir target/counter
scripts/measure.py target/counter/release/tuxctl --label dev
```

A separate target directory keeps the normal release build unchanged.

All three scripts pass anything after `--` to `tuxctl`.

## Guards

`measure.py --check` needs a `redraw-counter` build and fails when a structural
guard fails. The limits come from constants in `src/main.rs`, not from machine
speed, so they hold on shared CI runners:

| Guard | Limit | Reason |
| --- | --- | --- |
| Rapid hover redraws/s | ≤ 31 | Hover redraws are coalesced to one per 33 ms. |
| Log storm redraws/s | ≤ 21 | Background redraws are limited to one per 50 ms. |
| Overview and Processes idle redraws/s | ≤ 3.5 at `1s`, ≤ 9.5 at `250ms` | At most one redraw per collector update (metrics and network every interval, processes at most once per second), plus 0.5, never one per 250 ms tick. The limit follows `--interval` after `--`. |
| Startup RSS | ≤ 16 MiB | Catastrophe limit (about 3× a typical desktop). |
| Storm input latency (median) | ≤ 1000 ms | Catastrophe limit. |
| Exit | `q` exits with status 0 | Liveness. |

CPU % and absolute RSS are reported, never enforced: they depend on the machine.
Compare them locally against `docs/performance.md`.

## Measurement notes

- CPU % comes from `/proc/<pid>/stat` clock ticks (usually 10 ms), so short runs
  of an idle process are coarse: at 0.5 % over 20 s, one tick is 10 % of the
  reading.
- Logs idle redraws depend on how much the host journal is writing.
- With transparent huge pages set to `always`, a run can occasionally show
  about 2 MiB more RSS; the JSON output records `AnonHugePages` so such runs
  can be recognized.
- The first minutes include warm-up (command cache, hardware discovery); judge
  memory growth from minute 5.
- The redraw counter writes one small file per render; the effect is negligible
  at these rates, and the JSON output records which build was measured.

Safety notes:

- `smoke.py` sends signals only to a `sleep` process and to `tuxctl` instances
  that it starts itself.
- The log storms in `measure.py` write 200 messages per second through a single
  `logger` process, about 10 000 messages per run at the default `--secs 20`.
  The messages are tagged `tuxctl-storm`.
