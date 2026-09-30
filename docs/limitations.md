# Terminal support and limitations

## Terminal size

The normal interface needs at least **40 columns × 15 rows**. Below that,
`tuxctl` shows a terminal-too-small message instead.

Above it, layouts adapt: on narrow terminals the tab bar uses short labels
(`Ovr Proc Svc Logs Net`), and Overview cards that do not fit are left out in
priority order rather than drawn empty (see [Overview](overview.md#layout)).
Long values may be cut short in narrow layouts; there is no horizontal
scrolling.

## Colors

`tuxctl` picks true color, 256 colors or the 16 basic colors from `COLORTERM`
and `TERM` at startup (see [Overview](overview.md#colors)).

## Known limitations

- **Linux only:** `tuxctl` relies on Linux `/proc`, `/sys`, systemd tools and
  Linux-specific process signaling.
- **systemd:** the Services and Logs screens need `systemctl` and
  `journalctl`.
- **NVIDIA GPUs:** on the proprietary driver they need a glibc build; the
  static release binaries cannot load NVML.
- **Hardware hotplug:** hardware is discovered at startup; newly attached
  devices appear after a restart.
- **Permissions:** signaling other users' or privileged processes follows the
  normal Linux permission rules.
- **pidfds:** signaling needs pidfd support; `tuxctl` does not fall back to
  PID-only signaling.
