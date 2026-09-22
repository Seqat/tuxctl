#!/usr/bin/env python3
"""Record tuxctl RSS while it idles on Overview.

Usage: scripts/rss.py BINARY MINUTES [--every SECS] [--json PATH]
                      [--max-growth KIB --after MIN] [-- TUXCTL_ARGS...]

With --max-growth, exits non-zero if RSS grew by more than KIB between minute
--after (default 5; skips warm-up) and the end of the run.
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptyrun import Tui  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary")
    parser.add_argument("minutes", type=float)
    parser.add_argument("--every", type=float, default=60.0, metavar="SECS", help="sample period")
    parser.add_argument("--json", metavar="PATH", help="write samples as JSON")
    parser.add_argument("--max-growth", type=int, metavar="KIB", help="fail above this growth")
    parser.add_argument("--after", type=float, default=5.0, metavar="MIN", help="growth baseline")
    argv = sys.argv[1:]
    split = argv.index("--") if "--" in argv else len(argv)
    args = parser.parse_args(argv[:split])
    extra = argv[split + 1 :]

    tui = Tui([args.binary, *extra], settle=3.0)
    samples = [(0.0, tui.rss_kib())]
    print(f"startup: {samples[0][1]} kB", flush=True)
    elapsed = 0.0
    while elapsed < args.minutes * 60:
        tui.pump(args.every)
        elapsed += args.every
        if not tui.alive():
            sys.exit(f"tuxctl exited after {elapsed / 60:.1f} min")
        samples.append((elapsed / 60, tui.rss_kib()))
        print(f"{round(elapsed / 60, 2):g} min: {samples[-1][1]} kB", flush=True)
    tui.quit()

    baseline = next((rss for minute, rss in samples if minute >= args.after), samples[-1][1])
    growth = samples[-1][1] - baseline
    print(f"growth since minute {args.after:g}: {growth} kB")

    if args.json:
        with open(args.json, "w") as out:
            json.dump({
                "args": extra,
                "samples": [{"minute": round(minute, 3), "rss_kib": rss} for minute, rss in samples],
                "growth_after_minute": args.after,
                "growth_kib": growth,
            }, out, indent=2)
            out.write("\n")

    if args.max_growth is not None and growth > args.max_growth:
        sys.exit(f"RSS grew {growth} kB since minute {args.after:g} (limit {args.max_growth} kB)")


if __name__ == "__main__":
    main()
