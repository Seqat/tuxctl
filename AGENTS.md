# tuxctl Contributor Guide

## Product and scope

`tuxctl` is a Linux-native control-center TUI written in Rust with Ratatui and
Crossterm. Use Tokio only where asynchronous work is genuinely useful. Planned
screens are Overview, Processes, Services, Logs, and Network. Implement only
the modules needed by the current work; do not scaffold the full application
in advance.

## Architecture

Keep terminal input, application behavior, Linux data collection, and UI
rendering separated:

```text
Terminal Events
  |-- Keyboard
  |-- Mouse
  |-- Resize
  `-- Tick
        |
        v
      Action
        |
        v
       App
      /   \
     v     v
Linux Data UI
```

- Translate raw input events into semantic `Action` values before updating the
  application.
- Centralize application state in `App`. State transitions should consume
  actions rather than depend directly on Crossterm events.
- Keep Linux data collection separate from rendering. The UI receives prepared
  state or snapshots and must never perform blocking system calls.
- Keep rendering deterministic and side-effect free where practical.
- Use Tokio only for work that benefits from concurrency or asynchronous I/O;
  do not make the whole design async by default.

Use this structure as a direction, not as a requirement to create empty files:

```text
src/
|-- main.rs
|-- app.rs
|-- action.rs
|-- event.rs
|-- ui/
|   |-- mod.rs
|   |-- layout.rs
|   `-- components/
`-- linux/
    |-- mod.rs
    |-- cpu.rs
    |-- memory.rs
    `-- process.rs
```

## Interaction and terminal behavior

- Every important action must be accessible by keyboard.
- Mouse support may enhance keyboard controls but must never replace them.
- Treat terminal resize as a normal event and recompute layout safely.
- Never hard-code terminal coordinates. Derive geometry from the current frame
  area and handle very small terminal sizes without panicking or underflowing.

## Linux integration

- Prefer `/proc`, `/sys`, system APIs, or other structured interfaces over
  repeatedly spawning shell commands.
- Treat processes, services, devices, files, and other Linux resources
  disappearing between reads as normal. Return partial or refreshed data where
  sensible instead of panicking.
- Keep parsing and collection logic independent from Ratatui types so it can be
  tested without a terminal.
- Do not block the rendering loop. Perform potentially slow collection outside
  rendering and pass results back through application state and actions.

## Implementation guidelines

- Prefer simple, explicit Rust over premature abstraction.
- Avoid unnecessary dependencies. Use the standard library and existing
  dependencies when they are sufficient; explain any new crate's concrete
  benefit.
- Add focused tests for parsing, filtering, geometry, and state transitions
  where useful.
- Preserve a responsive event loop and avoid unbounded work per tick.

## Standard commands

Run these as applicable before handing off changes:

```sh
cargo fmt
cargo check
cargo clippy --all-targets
cargo test
cargo run
```
