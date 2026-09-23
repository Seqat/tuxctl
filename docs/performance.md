# Performance

Reference measurements for `tuxctl`, how they are made, and how releases
compare. These numbers come from one machine; they are a baseline for
regressions, not a guarantee for other systems. The limits that CI enforces
on every push are listed in [`scripts/README.md`](../scripts/README.md#guards).

## Reference machine

- AMD Ryzen 5 7500F (6 cores, 12 threads), CachyOS, Linux 7.2.6
- CPU governor and platform profile: `performance`; transparent huge pages: `always`
- Rust 1.98.1
- `tuxctl` runs in a 160×50 pseudo-terminal driven by `scripts/ptyrun.py`, at
  the default 1 s interval. The cost of a real terminal emulator is not
  included.

## Method

```sh
cargo build --release --locked --features redraw-counter --target-dir target/counter
scripts/measure.py target/counter/release/tuxctl --json run1.json   # three runs, report the median
cargo build --release --locked
scripts/rss.py target/release/tuxctl 15 --json rss.json
```

Each `measure.py` scenario lasts 20 s. CPU is the percentage of one core
(user + system time from `/proc/<pid>/stat`); RSS is `VmRSS`. The storm writes
200 journal messages per second.

Comparing two versions: run `measure.py` three times on each, on the same
machine and power profile. The noise band of a metric is the spread
(max − min) of the older version's runs. A newer median is unchanged if it is
within the larger of twice that band and a floor of 0.15 CPU percentage
points, 0.3 redraws/s, or 256 KiB. After minute 5, RSS should grow by at most
128 KiB over a 15-minute run.

## v0.2.7

Release binary: `x86_64-unknown-linux-musl`, statically linked, 1.39 MB.

| Scenario | CPU % | Redraws/s | RSS |
| --- | --- | --- | --- |
| Overview, idle | 0.60 | 1.20 | 2.3 MiB |
| Processes, idle | 0.65 | 2.00 | 2.4 MiB |
| Logs, idle | 0.55 | 0.00 | 2.5 MiB |
| Logs, 200 journal messages/s | 0.80 | 3.87 | 2.8 MiB |
| Mouse hover at 240 Hz | 1.29 | 13.41 | 2.7 MiB |

- Startup RSS: 2.2 MiB. After 15 minutes on Overview: 2.3 MiB, unchanged
  since minute 5.
- Time to the first frame: 2 ms. Input latency during the log storm: 0.7 ms.
- RSS change over two further 15 s storms with a full log buffer: within ±60 KiB.

A `cargo install` build (glibc, dynamically linked, 1.27 MB) measured
0.55 / 0.55 / 0.50 / 0.67 / 1.10 % CPU for the same scenarios, with the same
redraw rates. Its RSS is about 4.8 MiB at startup and 5.2 MiB after
15 minutes (+4 KiB since minute 5). A dynamically linked process also counts the pages of the shared C library
it maps, so RSS is higher than for the static build.

## History

Same machine, glibc builds, `measure.py` as of v0.2.7, median of three runs.

| Scenario | v0.2.5 CPU % | v0.2.7 CPU % | v0.2.5 redraws/s | v0.2.7 redraws/s |
| --- | --- | --- | --- | --- |
| Overview, idle | 0.55 | 0.55 | 1.40 | 1.25 |
| Processes, idle | 0.60 | 0.55 | 2.00 | 2.00 |
| Logs, idle | 0.55 | 0.50 | 0.00 | 0.05 |
| Logs, 200 journal messages/s | 0.67 | 0.67 | 3.87 | 3.87 |
| Mouse hover at 240 Hz | 1.20 | 1.10 | 13.56 | 13.54 |

| | v0.2.5 | v0.2.7 |
| --- | --- | --- |
| Startup RSS | 5.1 MiB | 4.8 MiB |
| RSS after 15 minutes | 5.4 MiB¹ | 5.2 MiB |
| Binary size | 1.94 MB | 1.27 MB |

¹ Measured before v0.2.7 with the same `rss.py` run: 5 496 kB, flat after
minute 4.

v0.2.7 changes no collector cadence or rendering path; the CPU and redraw
differences above are within the noise band. The smaller binary and the
roughly 200 KiB lower RSS come from the release profile (LTO, one codegen
unit, stripped symbols).

Overview idle redraws vary between runs (1.0 to 1.4 per second): the metrics,
process, and network collectors each publish once per second, and their
updates share a redraw only when they arrive close together.

## Sources of noise

- **Tick resolution:** CPU time is counted in 10 ms clock ticks, so a 20 s idle
  scenario at 0.5 % is only about 10 ticks.
- **Background load and power profile:** keep them the same across compared runs.
- **Journal traffic:** Logs idle redraws depend on how much the host writes to
  the journal.
- **Transparent huge pages:** with THP set to `always`, a run can show about
  2 MiB more RSS. `measure.py` and `rss.py` record `AnonHugePages` to identify
  such runs.
- **Warm-up:** the command cache and hardware discovery fill during the first
  minutes, so judge memory growth from minute 5 on.
- **CI runners** vary by CPU model and neighbours: idle CPU on GitHub-hosted
  runners ranged from 0.25 % to 0.62 % for the same scenario. CI therefore
  enforces only redraw limits, liveness, and catastrophe limits, and reports
  CPU and RSS.
