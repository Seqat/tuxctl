# Controls

Every action has a key; the mouse offers the common ones too. `?` shows the
keys for the current screen inside `tuxctl`.

## Global Controls

| Key | Action |
| --- | --- |
| `1` - `5` | Switch directly to tab: Overview, Processes, Services, Logs, Network |
| `Tab` / `Shift+Tab` | Next / previous tab |
| `→` / `←` | Next / previous tab |
| `?` | Toggle Help dialog |
| `+` / `-` | Longer / shorter sampling interval (`250ms` to `60s`, shown as `⟳` in the top-right corner) |
| `Esc` | Dismiss dialog / clear the search, then the view filter / open the main menu |
| `q` | Open the main menu on Exit; `Enter` or `q` again quits |
| `Ctrl+C` | Quit immediately, from anywhere |

## Navigation & Common Actions

| Key | Action |
| --- | --- |
| `↑` / `k` | Move selection up |
| `↓` / `j` | Move selection down |
| `PageUp` / `PageDown` | Move selection by page |
| `Home` / `End` | Jump to first / last item |
| `/` | Begin search / filter; `↑` / `↓` and `PageUp` / `PageDown` move through the matches while typing |
| `Enter` | Open detailed inspection |

## Processes

| Key | Action |
| --- | --- |
| `c` | Sort by CPU % |
| `m` | Sort by Memory |
| `p` | Sort by PID |
| `n` | Sort by Name |
| `T` / `Shift+T` | Request `SIGTERM` for selected process |
| `K` / `Shift+K` | Request `SIGKILL` for selected process |
| `P` / `Shift+P` | Pin / unpin the selected process (up to 8) |
| `Shift+↑` / `Shift+↓` | Move the selected pinned process up / down (`Alt+↑` / `Alt+↓` also work) |
| `v` | Hide / show kernel threads |

Repeated sort commands toggle the sort direction. Pinned processes stay at the top in the order you give them, marked with `*`; sorting applies to the rows below them. While a search is active, pinned processes that do not match stay visible but dimmed. A pinned process that exits is shown as `exited` for a few seconds and then removed; it can never be signaled.

### Signal Confirmation

| Key | Action |
| --- | --- |
| `Tab` / `←` / `→` / `h` / `l` | Move focus between `Cancel` and confirmation |
| `Enter` | Execute focused action |
| `Esc` | Cancel and close |

### Main Menu

`Esc` opens the main menu when there is no dialog, search, or view filter to clear.

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move between About and Exit (`k` / `j` also work) |
| `Enter` | Open About, or exit `tuxctl` |
| `q` | Exit `tuxctl` |
| `Esc` | Close the menu (from About, go back to the menu) |

## Services

| Key | Action |
| --- | --- |
| `r` | Request an immediate service refresh |
| `v` | Cycle the view: all units → loaded units (hide `not-found`) → failed units |

## Logs

| Key | Action |
| --- | --- |
| `f` | Toggle follow mode |
| `Space` | Pause / resume |
| `v` | Cycle the minimum priority: all → notice → warning → error |

## Mouse Controls

- **Tabs:** Click a tab title to switch screens.
- **Selection:** Click rows in Processes, Services, Logs, or Network.
- **Scrolling:** Use the mouse wheel over list/table areas.
- **Process sorting:** Click `PID`, `NAME`, `CPU`, or `MEMORY` headers.
- **Pinned processes:** Click `▲` / `▼` at the end of a pinned row to move it (shown when there are at least two pins and the terminal is wide enough).
- **Confirmation dialogs:** Click `Cancel` or the confirmation action.
- **Main menu:** Click `About` or `Exit`.
- **Sampling interval:** Click `[-]` / `[+]` next to `⟳` in the top-right corner.
