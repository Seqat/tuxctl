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

## v0.3.0

Release binary: `x86_64-unknown-linux-musl`, statically linked, 1.45 MB.
Median of three runs, alternated with three runs of the v0.2.7 musl binary
in the same session (in brackets).

| Scenario | CPU % | Redraws/s | RSS |
| --- | --- | --- | --- |
| Overview, idle | 0.60 (0.60) | 1.10 (1.00) | 2.3 MiB |
| Processes, idle | 0.75 (0.70) | 2.00 (2.00) | 2.4 MiB |
| Logs, idle | 0.60 (0.55) | 0.00 (0.00) | 2.5 MiB |
| Logs, 200 journal messages/s | 0.87 (0.87) | 3.87 (3.87) | 2.8 MiB |
| Mouse hover at 240 Hz | 1.39 (1.29) | 13.54 (13.43) | 2.8 MiB |

- Every difference is within the noise band: the v0.2.7 hover runs alone
  spread by 0.20 points.
- Startup RSS: 2.3 MiB (v0.2.7: 2.2–2.3 MiB). After 15 minutes on Overview:
  2.3 MiB, unchanged since minute 5.
- Time to the first frame: 2 ms. Input latency during the log storm: 0.9 ms
  (0.75 ms).
- The v0.2.7 figures below came from an earlier session and read about
  0.05–0.1 points lower on some scenarios than v0.2.7 measured again here;
  compare versions only within one session.

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

| Scenario | v0.2.5 CPU % | v0.2.7 CPU % | v0.3.0 CPU % | v0.2.5 redraws/s | v0.2.7 redraws/s | v0.3.0 redraws/s |
| --- | --- | --- | --- | --- | --- | --- |
| Overview, idle | 0.55 | 0.55 | 0.55 | 1.40 | 1.25 | 1.10 |
| Processes, idle | 0.60 | 0.55 | 0.60 | 2.00 | 2.00 | 2.00 |
| Logs, idle | 0.55 | 0.50 | 0.55 | 0.00 | 0.05 | 0.10 |
| Logs, 200 journal messages/s | 0.67 | 0.67 | 0.67 | 3.87 | 3.87 | 3.93 |
| Mouse hover at 240 Hz | 1.20 | 1.10 | 1.19 | 13.56 | 13.54 | 13.42 |

| | v0.2.5 | v0.2.7 | v0.3.0 |
| --- | --- | --- | --- |
| Startup RSS | 5.1 MiB | 4.8 MiB | 4.9 MiB |
| RSS after 15 minutes | 5.4 MiB¹ | 5.2 MiB | 5.1 MiB² |
| Binary size | 1.94 MB | 1.27 MB | 1.33 MB |

¹ Measured before v0.2.7 with the same `rss.py` run: 5 496 kB, flat after
minute 4.
² 5 264 kB, +4 KiB since minute 5; v0.2.7 measured 5 200 kB (+0 KiB) in the
same session.

v0.3.0 was measured interleaved with three fresh v0.2.7 runs on the same day
(v0.2.7: 0.55 / 0.60 / 0.50 / 0.67 / 1.09 % CPU, 1.10 / 2.00 / 0.00 / 3.93 /
13.48 redraws/s); every difference is within the noise band. Time to the first
frame stayed at about 1.9 ms and storm input latency at about 0.5 ms. Single
runs of each intermediate v0.3.0 commit stayed within the same band, so no
item stands out. The sanitization pass over every frame does not show in the
hover scenario, the most render-heavy one. The binary grew by 4.8 % (disk I/O,
pinning, filters, menu, sparklines). The Logs idle redraws come from host
journal traffic, as noted below.

### Shortest interval (`--interval 250ms`)

One run each, glibc builds; the idle guard is 9.5 redraws/s at this interval.

| Scenario | v0.2.7 CPU % | v0.3.0 CPU % | v0.2.7 redraws/s | v0.3.0 redraws/s |
| --- | --- | --- | --- | --- |
| Overview, idle | 0.75 | 0.80 | 4.00 | 4.00 |
| Processes, idle | 0.80 | 0.85 | 3.80 | 3.85 |
| Logs, idle | 0.65 | 0.70 | 0.15 | 0.00 |
| Logs, 200 journal messages/s | 0.80 | 0.80 | 3.87 | 3.87 |
| Mouse hover at 240 Hz | 1.29 | 1.39 | 13.48 | 13.58 |

Four times as many metrics and network samples cost about 0.2 CPU percentage
points on the idle screens and four redraws per second on Overview: the
metrics and network updates usually arrive together and share one redraw.

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
