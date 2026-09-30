# Processes

![The Processes screen](screenshots/processes.png)

A live list of every process from `/proc`, with the total CPU and RAM use
above it.

- **Columns:** PID, name, CPU percentage and resident memory. Sort with `c`,
  `m`, `p` or `n`, or by clicking a column header; sorting again reverses the
  order.
- **Search:** `/` filters by name, PID or command line, case-insensitively.
- **Kernel threads:** `v` hides or shows them.
- **Details:** `Enter` opens the selected process: its name, command line,
  state, parent PID, CPU and memory, and whether it is a kernel thread.

![Process details](screenshots/process_about.png)

## Pinning

`P` pins the selected process to the top of the list; up to 8 can be pinned,
in your own order (`Shift+↑` / `Shift+↓`, or the `▲` / `▼` controls at the end
of a pinned row). Sorting applies to the rows below them. During a search,
pinned processes that do not match stay visible but dimmed. Pinned processes
also appear on the [Overview](overview.md).

A pin follows the process identity, `(PID, start time)`, so a new process
that reuses the PID never inherits it. A pinned process that exits is shown as
`exited` for a few seconds and cannot be signaled.

## Signals

`T` requests `SIGTERM` and `K` requests `SIGKILL` for the selected process.
Nothing is sent until you confirm, and `Cancel` is the default.

![Signal confirmation](screenshots/process_terminate.png)

When you confirm, `tuxctl` opens a pidfd for the process and checks that its
start time still matches the one you saw before sending the signal through
that pidfd. A process that exited, or a new process that took its PID, is
never signaled. If pidfds are not available, the signal is refused.

Signaling other users' or privileged processes follows the normal Linux
permission rules.
