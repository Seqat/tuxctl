#!/usr/bin/env python3
"""Measure tuxctl CPU %, RSS and redraws/s for standard scenarios.

Usage: scripts/measure.py BINARY [--label NAME] [--secs N] [-- TUXCTL_ARGS...]

Redraws are only counted when BINARY was built with `--features redraw-counter`;
otherwise that column reads 0.
"""

import argparse
import os
import select
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptyrun import Tui  # noqa: E402

TICKS_PER_SECOND = os.sysconf("SC_CLK_TCK")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary")
    parser.add_argument("--label", default="tuxctl")
    parser.add_argument("--secs", type=float, default=20.0, help="seconds per scenario")
    argv = sys.argv[1:]
    split = argv.index("--") if "--" in argv else len(argv)
    args = parser.parse_args(argv[:split])
    extra = argv[split + 1 :]

    with tempfile.TemporaryDirectory() as scratch:
        counter = os.path.join(scratch, "redraws")
        tui = Tui([args.binary, *extra], env={"TUXCTL_REDRAW_LOG": counter}, settle=3.0)

        def redraws():
            try:
                return int(open(counter).read() or 0)
            except (OSError, ValueError):
                return 0

        rows = []

        def record(name, run):
            start, ticks, renders = time.time(), tui.cpu_ticks(), redraws()
            run()
            elapsed = time.time() - start
            cpu = (tui.cpu_ticks() - ticks) / TICKS_PER_SECOND / elapsed * 100
            rows.append((name, f"{cpu:.2f}", f"{tui.rss_kib()} kB", f"{(redraws() - renders) / elapsed:.2f}"))

        print(f"[{args.label}] startup RSS: {tui.rss_kib()} kB")
        for name, key in [("overview idle", "1"), ("processes idle", "2"), ("logs idle", "4")]:
            tui.send(key, 1.5)
            record(name, lambda: tui.pump(args.secs))

        def storm():
            # Rate-limited (~200 msg/s) so the user's journal is not flooded.
            logger = subprocess.Popen(
                ["sh", "-c", "while :; do logger -t tuxctl-storm storm; sleep 0.005; done"],
                start_new_session=True,
            )
            try:
                tui.pump(min(args.secs, 15))
            finally:
                os.killpg(logger.pid, 15)
                logger.wait()

        record("logs storm (~200 msg/s)", storm)

        def hover():
            end = time.time() + args.secs / 2
            while time.time() < end:
                for x in list(range(1, 80)) + list(range(80, 1, -1)):
                    os.write(tui.fd, f"\x1b[<35;{x};2M".encode())
                    time.sleep(1 / 240)
                    ready, _, _ = select.select([tui.fd], [], [], 0)
                    if ready:
                        os.read(tui.fd, 65536)

        record("rapid hover 240 Hz", hover)
        tui.quit()

    print(f"[{args.label}] scenario | CPU % | RSS | redraws/s")
    for row in rows:
        print(" | ".join(row))


if __name__ == "__main__":
    main()
