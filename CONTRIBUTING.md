# Contributing to tuxctl

Thanks for your interest. `tuxctl` is a small, deliberately restrained tool
maintained in spare time, so a short discussion before a larger change saves
everyone work.

## Before you start

- **Bugs:** open an issue with `tuxctl --version`, `tuxctl --check`, your
  distribution and terminal, and the steps to reproduce.
- **Features:** open an issue first. `tuxctl` favors a readable interface,
  low idle CPU and memory, and few dependencies over a long feature list, so
  some good ideas will not fit.
- **Security problems:** do not open an issue; follow the
  [security policy](SECURITY.md).

## Building and checking

```sh
cargo build
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

These run on every pull request, together with the minimum supported Rust
version (1.88), a pseudo-terminal smoke test and redraw-rate guards; see
[Development](https://seqat.github.io/tuxctl/development.html) for the scripts.

## Branches and commits

- Work on a branch from `dev` and open the pull request against `dev`; `main`
  only receives releases.
- One logical change per commit, with a short prefix: `feat:`, `fix:`,
  `perf:`, `docs:`, `ci:`, `refactor:`, `test:` or `chore:`. Say why in the
  body when it is not obvious.
- User-visible changes get an entry under `[Unreleased]` in
  [CHANGELOG.md](CHANGELOG.md).

## Code guidelines

- **Rendering reads cached state only.** `/proc`, `/sys`, `systemctl` and
  `journalctl` are read by background collectors, never while drawing.
- **Redraw only when something visible changed.** Mouse movement, hidden
  screens and background updates must not force redraws.
- **Everything stays bounded:** histories, buffers, queues and anything read
  from other users' data.
- **No new dependency** without a clear reason and a look at its cost.
- **No panics outside tests:** handle errors instead (`clippy` denies
  `unwrap`, `expect`, `panic!` and `unreachable!` there).
- **Every `unsafe` block** carries a `// SAFETY:` comment explaining why it is
  sound (`clippy` enforces it).
- Keyboard first: every action needs a key; mouse support is welcome on top.

## Tests

Unit tests live with the code in a `#[cfg(test)] mod tests` module. Two rules
decide where that module goes:

1. **Split a large test module.** When a file is longer than 500 lines and its
   tests make up at least half of it, or its tests alone are longer than
   1000 lines, the tests move to a sibling `tests.rs` file
   (`src/ui/overview.rs` → `src/ui/overview/tests.rs`), declared as
   `#[cfg(test)] mod tests;`.
2. **Split stays split.** Tests that were moved out are not moved back, even if
   the file later falls below these limits.

`tests/` holds only tests that run the compiled binary.

When you change a key binding, update [docs/controls.md](docs/controls.md): a
test checks its tables against the real bindings.

## Documentation

The documentation site is built from [`docs/`](docs/) with mdBook
(`mdbook serve docs --open`). Keep the README short and put details there.
