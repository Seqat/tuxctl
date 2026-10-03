# Logs

![The Logs screen](screenshots/logs.png)

The systemd journal, streamed from `journalctl`. It starts the first time the
Logs screen is opened, with the last 200 entries.

- **Time:** entries show the local time (`HH:MM:SS`); the detail view has the
  full date and UTC offset.
- **Severity:** errors, warnings, informational and debug messages are styled
  differently.
- **Follow and pause:** `f` follows new entries; `Space` pauses and resumes.
- **Search:** `/` filters the entries.
- **Priority:** `v` cycles the minimum priority: all, notice, warning, error.
- **Details:** `Enter` shows the whole, multi-line message.

![Log details](screenshots/logs_detail.png)

The screen keeps the last 2000 entries. Under a burst of messages, entries that
cannot be taken in time are counted as dropped rather than piling up, and
reading the journal never holds up the keyboard.
