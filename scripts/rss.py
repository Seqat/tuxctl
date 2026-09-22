#!/usr/bin/env python3
"""Record tuxctl RSS once per minute while it idles on Overview.

Usage: scripts/rss.py BINARY MINUTES [-- TUXCTL_ARGS...]
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptyrun import Tui  # noqa: E402


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    binary, minutes = sys.argv[1], int(sys.argv[2])
    extra = [arg for arg in sys.argv[3:] if arg != "--"]

    tui = Tui([binary, *extra], settle=3.0)
    print(f"startup: {tui.rss_kib()} kB", flush=True)
    for minute in range(1, minutes + 1):
        tui.pump(60)
        if not tui.alive():
            sys.exit(f"tuxctl exited after {minute - 1} min")
        print(f"{minute} min: {tui.rss_kib()} kB", flush=True)
    tui.quit()


if __name__ == "__main__":
    main()
