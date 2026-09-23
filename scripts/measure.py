#!/usr/bin/env python3
"""Measure tuxctl CPU %, RSS and redraws/s for standard scenarios.

Usage: scripts/measure.py BINARY [--label NAME] [--secs N] [--json PATH] [--check]
                                 [-- TUXCTL_ARGS...]

Redraws are only counted when BINARY was built with `--features redraw-counter`;
otherwise that column reads 0. `--check` needs such a build and exits non-zero
when a structural guard (see GUARDS) fails.
"""

import argparse
import datetime
import json
import os
import platform
import select
import statistics
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptyrun import LogStorm, Tui  # noqa: E402

TICKS_PER_SECOND = os.sysconf("SC_CLK_TCK")
COLS, ROWS = 160, 50
STORM_RATE = 200
# Fixed, independent of --secs: 3000 messages overfill the 2000-entry log ring.
SATURATION_SECS = 15
LATENCY_SAMPLES = 5

# (scenario, max redraws/s, reason). Limits come from constants in src/main.rs,
# not from machine speed, so they hold on any runner.
GUARDS = [
    ("rapid hover 240 Hz", 31, "33 ms hover coalescing"),
    ("logs storm", 21, "50 ms background frame limit"),
]
# At most one render per collector update (metrics and network every interval,
# processes at most once per second), never one per 250 ms tick; see
# idle_limit(). At the default 1 s interval the limit is 3.5.
IDLE_GUARDS = ["overview idle", "processes idle"]
INTERVALS = {"250ms": 0.25, "500ms": 0.5, "1s": 1.0, "2s": 2.0, "5s": 5.0,
             "10s": 10.0, "30s": 30.0, "60s": 60.0}
# Catastrophe limits only; absolute numbers are compared locally.
MAX_STARTUP_RSS_KIB = 16 * 1024
MAX_STORM_LATENCY_MS = 1000


def sampling_interval(extra):
    """The --interval passed to tuxctl in seconds (default 1 s), None if unknown."""
    value = "1s"
    for index, arg in enumerate(extra):
        if arg == "--interval" and index + 1 < len(extra):
            value = extra[index + 1]
        elif arg.startswith("--interval="):
            value = arg.split("=", 1)[1]
    return INTERVALS.get(value)


def idle_limit(interval):
    """Collector updates per second at `interval`, plus 0.5 of slack."""
    return round(2 / interval + 1 / max(interval, 1.0) + 0.5, 2)


def environment(binary, label, secs, extra, counter_build):
    version = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    cpu_model = platform.machine()
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    return {
        "label": label,
        "version": version,
        "time_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "kernel": platform.release(),
        "machine": platform.machine(),
        "cpu_model": cpu_model,
        "cpus": os.cpu_count(),
        "pty": f"{COLS}x{ROWS}",
        "secs_per_scenario": secs,
        "tuxctl_args": extra,
        "counter_build": counter_build,
        "binary_size": os.path.getsize(binary),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary")
    parser.add_argument("--label", default="tuxctl")
    parser.add_argument("--secs", type=float, default=20.0, help="seconds per scenario")
    parser.add_argument("--json", metavar="PATH", help="write results as JSON")
    parser.add_argument("--check", action="store_true", help="fail on structural guards")
    argv = sys.argv[1:]
    split = argv.index("--") if "--" in argv else len(argv)
    args = parser.parse_args(argv[:split])
    extra = argv[split + 1 :]

    with tempfile.TemporaryDirectory() as scratch:
        counter = os.path.join(scratch, "redraws")
        tui = Tui([args.binary, *extra], cols=COLS, rows=ROWS,
                  env={"TUXCTL_REDRAW_LOG": counter}, settle=3.0)
        counter_build = os.path.exists(counter)
        if args.check and not counter_build:
            tui.quit()
            sys.exit("--check needs a binary built with --features redraw-counter")

        def redraws():
            try:
                return int(open(counter).read() or 0)
            except (OSError, ValueError):
                return 0

        rows = []
        failures = []

        def record(name, run):
            start, ticks, renders = time.time(), tui.cpu_ticks(), redraws()
            run()
            elapsed = time.time() - start
            if not tui.alive():
                failures.append(f"tuxctl exited during '{name}'")
                return False
            rows.append({
                "scenario": name,
                "secs": round(elapsed, 2),
                "cpu_pct": round((tui.cpu_ticks() - ticks) / TICKS_PER_SECOND / elapsed * 100, 2),
                "rss_kib": tui.rss_kib(),
                "redraws_per_s": round((redraws() - renders) / elapsed, 2),
            })
            return True

        metrics = {
            "startup_rss_kib": tui.rss_kib(),
            "startup_anon_huge_kib": tui.anon_huge_kib(),
            "first_frame_ms": tui.first_frame_ms,
        }

        def storm_for(seconds):
            with LogStorm(STORM_RATE):
                tui.pump(seconds)

        def hover():
            end = time.time() + args.secs / 2
            while time.time() < end:
                for x in list(range(1, 80)) + list(range(80, 1, -1)):
                    os.write(tui.fd, f"\x1b[<35;{x};2M".encode())
                    time.sleep(1 / 240)
                    ready, _, _ = select.select([tui.fd], [], [], 0)
                    if ready:
                        os.read(tui.fd, 65536)

        def phases():
            for name, key in [("overview idle", "1"), ("processes idle", "2"), ("logs idle", "4")]:
                tui.send(key, 1.5)
                if not record(name, lambda: tui.pump(args.secs)):
                    return
            if not record("logs storm", lambda: storm_for(min(args.secs, 15))):
                return
            if not record("rapid hover 240 Hz", hover):
                return

            # After the rate windows, so their input cannot skew the redraw guards.
            tui.send("4", 1.0)
            storm_for(SATURATION_SECS)
            saturated = tui.rss_kib()
            storm_for(SATURATION_SECS)
            metrics["storm_rss_growth_kib"] = tui.rss_kib() - saturated

            # Switching tabs restyles the whole "Processes" tab label, so the
            # word is always redrawn in one piece.
            samples = []
            with LogStorm(STORM_RATE):
                tui.pump(1.0)
                for _ in range(LATENCY_SAMPLES):
                    for key in (b"2", b"4"):
                        os.write(tui.fd, key)
                        samples.append(tui.wait_for(b"Processes", 2.0))
                        tui.pump(0.3)
            answered = [sample for sample in samples if sample is not None]
            metrics["storm_latency_ms"] = round(statistics.median(answered), 2) if answered else None
            metrics["storm_latency_missed"] = len(samples) - len(answered)

        phases()
        metrics["final_anon_huge_kib"] = tui.anon_huge_kib() if tui.alive() else None
        metrics["exit_code"] = tui.quit()

    guards = []
    if counter_build:
        by_name = {row["scenario"]: row for row in rows}
        interval = sampling_interval(extra)
        idle = [] if interval is None else [
            (name, idle_limit(interval), f"one render per collector update at {interval:g} s, "
             "not per 250 ms tick") for name in IDLE_GUARDS]
        for name, limit, reason in GUARDS + idle:
            if name in by_name:
                value = by_name[name]["redraws_per_s"]
                guards.append((f"{name} redraws/s {value} <= {limit}", value <= limit, reason))
    latency = metrics.get("storm_latency_ms")
    guards += [
        (f"startup RSS {metrics['startup_rss_kib']} kB <= {MAX_STARTUP_RSS_KIB}",
         metrics["startup_rss_kib"] <= MAX_STARTUP_RSS_KIB, "catastrophe limit"),
        (f"storm input latency {latency} ms <= {MAX_STORM_LATENCY_MS}",
         latency is not None and latency <= MAX_STORM_LATENCY_MS, "catastrophe limit"),
        (f"'q' exits cleanly (exit {metrics['exit_code']})", metrics["exit_code"] == 0, "liveness"),
    ]
    guards += [(failure, False, "liveness") for failure in failures]

    print(f"[{args.label}] startup RSS: {metrics['startup_rss_kib']} kB")
    print(f"[{args.label}] scenario | CPU % | RSS | redraws/s")
    for row in rows:
        print(f"{row['scenario']} | {row['cpu_pct']:.2f} | {row['rss_kib']} kB | {row['redraws_per_s']:.2f}")
    print(f"[{args.label}] first frame: {metrics['first_frame_ms'] or 0:.1f} ms, "
          f"storm input latency (median): {latency} ms, "
          f"storm RSS growth with a full log ring: {metrics.get('storm_rss_growth_kib')} kB")
    for text, ok, reason in guards:
        print(f"{'PASS' if ok else 'FAIL'}  {text}  ({reason})")

    if args.json:
        result = {
            "environment": environment(args.binary, args.label, args.secs, extra, counter_build),
            "scenarios": rows,
            "metrics": metrics,
            "guards": [{"guard": text, "ok": ok, "reason": reason} for text, ok, reason in guards],
        }
        with open(args.json, "w") as out:
            json.dump(result, out, indent=2)
            out.write("\n")

    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as out:
            out.write(f"### tuxctl measurements ({args.label})\n\n")
            out.write("| Scenario | CPU % | RSS | Redraws/s |\n| --- | --- | --- | --- |\n")
            for row in rows:
                out.write(f"| {row['scenario']} | {row['cpu_pct']:.2f} | {row['rss_kib']} kB | "
                          f"{row['redraws_per_s']:.2f} |\n")
            out.write(f"\nStartup RSS {metrics['startup_rss_kib']} kB · storm input latency "
                      f"{latency} ms · storm RSS growth {metrics.get('storm_rss_growth_kib')} kB\n\n")
            for text, ok, reason in guards:
                out.write(f"- {'✅' if ok else '❌'} {text} ({reason})\n")
            out.write("\n")

    if args.check and not all(ok for _, ok, _ in guards):
        sys.exit(1)


if __name__ == "__main__":
    main()
