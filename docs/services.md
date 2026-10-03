# Services

![The Services screen](screenshots/services.png)

The systemd units from `systemctl`: unit, load state, active state, sub-state
and description.

- **States:** `● active`, `✖ failed`, `○ inactive`, `◌ activating`.
- **Search:** `/` filters units, case-insensitively.
- **View:** `v` cycles through all units, loaded units (hiding `not-found`),
  and failed units only.
- **Refresh:** `r` asks for a new listing at once.
- **Details:** `Enter` shows the selected unit. The screen is read-only; it
  never starts, stops or changes a unit.

Services are collected only while this screen is visible, and refreshed each
time it is opened, at most every 5 seconds. A `systemctl` call that hangs is
stopped after 10 seconds, so it cannot stall the screen or exit.
