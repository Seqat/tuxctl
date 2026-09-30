# Development

```sh
git clone https://github.com/Seqat/tuxctl.git
cd tuxctl
cargo run                    # debug build
cargo build --release        # optimized build in target/release/tuxctl
```

Work happens on the `dev` branch; `main` receives releases.

## Checks

These run on every push and pull request, and should pass before a change is
proposed:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The minimum supported Rust version is 1.88 (`cargo +1.88 check --all-targets`).

## Scripts

[`scripts/`](https://github.com/Seqat/tuxctl/tree/main/scripts) has the tools
used for releases and performance work:

- `smoke.py` drives the release binary in a pseudo-terminal and checks keys,
  resizing, exit and terminal restoration.
- `measure.py` measures CPU, memory and redraw rates in standard scenarios; with
  `--check` it enforces the guards CI uses.
- `rss.py` watches memory over a long run.

See [`scripts/README.md`](https://github.com/Seqat/tuxctl/blob/main/scripts/README.md)
for their options, and [Performance](performance.md) for how measurements are
made and compared.

## This documentation

The pages live in `docs/` and are built with [mdBook](https://rust-lang.github.io/mdBook/):

```sh
mdbook serve docs --open
```

The key tables on [Controls](controls.md) are checked by a test against the
real key bindings, so they cannot drift from the code.
