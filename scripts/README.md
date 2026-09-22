# Development scripts

Python 3 (standard library only) helpers that drive a release build of `tuxctl` in a
pseudo-terminal. They need Linux with systemd and are not run in CI.

| Script | Purpose |
| --- | --- |
| `smoke.py BINARY` | Runs the CLAUDE.md smoke list: tabs, mouse, resize down to 1×1, navigation, sorting, search, details, SIGKILL confirmation, Services, Logs, Help, quit and terminal restore. It exits with a non-zero status if any check fails. |
| `measure.py BINARY [--label NAME] [--secs N]` | Prints CPU %, RSS and redraws/s for idle Overview, Processes and Logs, a rate-limited log storm and rapid hover. |
| `rss.py BINARY MINUTES` | Prints RSS once a minute while tuxctl idles on Overview. |

`measure.py` counts redraws only when the binary is built with the development
feature. Without it, the redraws/s column is 0:

```sh
cargo build --release --features redraw-counter
scripts/measure.py target/release/tuxctl --label dev
cargo build --release   # rebuild without the counter afterwards
```

All three scripts pass anything after `--` to `tuxctl`.

Safety notes:

- `smoke.py` sends signals only to a `sleep` process that it starts itself.
- The log storm in `measure.py` writes about 200 messages per second to your
  journal for at most 15 seconds. The messages are tagged `tuxctl-storm`.
