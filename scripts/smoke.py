#!/usr/bin/env python3
"""Scripted smoke test of tuxctl in a pty (CLAUDE.md manual smoke list).

Usage: scripts/smoke.py BINARY [-- TUXCTL_ARGS...]

Sends SIGKILL only to a disposable `sleep` it starts itself. Exits non-zero on any
failure. Needs systemd (systemctl/journalctl) for the Services and Logs checks.
"""

import os
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptyrun import Tui  # noqa: E402

if len(sys.argv) < 2:
    sys.exit(__doc__)
ARGV = [sys.argv[1], *[arg for arg in sys.argv[2:] if arg != "--"]]
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


t = Tui(ARGV, cols=120, rows=40)
check("startup enters alternate screen + mouse capture", b"\x1b[?1049h" in t.all and b"\x1b[?1000h" in t.all)


def children(tui, name):
    return [pid for pid, comm in tui.session_processes() if comm == name]


check("no journalctl before the Logs tab is visited", not children(t, "journalctl"))

for key in "12345":
    t.send(key)
t.send("\t"); t.send("\x1b[Z"); t.send("\x1b[C"); t.send("\x1b[D")
check("tab navigation keys (1-5, Tab, Shift+Tab, arrows)", t.alive())
check("one journalctl after the Logs tab is visited", len(children(t, "journalctl")) == 1)

t.send("1")
out = b""
for x in range(2, 60, 3):
    out += t.click(x, 2, 0.15)
check("mouse tab clicks across the tab bar", t.alive() and len(out) > 0)

for cols, rows in [(40, 15), (39, 15), (40, 14), (20, 5), (1, 1), (41, 16), (200, 60), (120, 40)]:
    t.resize(cols, rows); t.pump(0.3)
check("resize sweep incl. below-minimum and 1x1", t.alive())
t.resize(39, 14); out = t.send("", 0.5)
check("below-minimum shows 'Terminal too small'", b"too small" in out)
t.resize(120, 40); t.pump(0.5)

t.send("2", 0.6)
for key in ["j", "j", "k", "\x1b[6~", "\x1b[5~", "\x1b[F", "\x1b[H"]:
    t.send(key, 0.15)
check("keyboard row navigation on Processes", t.alive())
for key in "cmpnc":
    t.send(key, 0.2)
check("keyboard sorting c/m/p/n", t.alive())
out = b""
for x in range(3, 118, 6):
    out += t.click(x, 5, 0.12)
check("mouse clicks along the Processes header row", t.alive())
for _ in range(5):
    t.wheel(40, 12)
for _ in range(5):
    t.wheel(40, 12, down=False)
check("mouse wheel over the process list", t.alive())
t.send("/"); t.send("zzzz-no-match")
out = t.send("", 0.3)
t.send("\x1b")
check("process search + Esc clear", t.alive())
t.send("\r", 0.4); out = t.send("\x1b", 0.3)
check("process detail open/close", t.alive())

# A disposable process is the only signal target; never a real process.
victim = subprocess.Popen(["sleep", "98766"])
t.pump(1.5)
t.send("/"); t.send("98766", 1.3); t.send("\r", 0.4); t.send("\x1b", 0.3)
out = t.send("K", 0.4)
check("SIGKILL confirmation dialog appears", b"SIGKILL" in out)
t.send("\x1b", 0.4)
time.sleep(0.3)
check("Esc cancels SIGKILL (victim alive)", victim.poll() is None)
t.send("K", 0.4); t.send("\r", 0.6)
check("Enter on default button cancels (victim alive)", victim.poll() is None)
t.send("K", 0.4); t.send("\t", 0.2); t.send("\r", 0.8)
time.sleep(0.3)
check("confirmed SIGKILL kills the verified process", victim.poll() == -9, f"returncode={victim.poll()}")
if victim.poll() is None:
    victim.kill()
t.send("\x1b")

t.send("3", 1.0)
for key in ["j", "j", "k", "r"]:
    t.send(key, 0.3)
t.send("/"); t.send("ssh"); t.send("\x1b"); t.send("\x1b")
check("services navigation, refresh, search", t.alive())

t.send("4", 1.0)
for key in ["f", "f", " ", " ", "k", "k", "j", "\x1b[F"]:
    t.send(key, 0.2)
t.send("\r", 0.3); t.send("\x1b", 0.3)
check("logs follow/pause/manual scroll/detail", t.alive())

t.send("5", 0.8)
t.send("j"); out = t.send("\r", 0.4)
check("network details open", t.alive() and len(out) > 0)
t.send("\x1b")

t.send("?", 0.4); t.send("?", 0.4)
check("help overlay toggle", t.alive())

t.send("2", 0.4); t.send("/"); t.send("q", 0.3)
check("'q' in search mode is text, not quit", t.alive())
t.all = b""
os.write(t.fd, b"\x03")
code = t.wait_exit()
tail = t.all[-400:]
check("Ctrl+C quits from search mode (exit 0)", code == 0, f"exit={code}")


def restored(output):
    return b"\x1b[?1049l" in output and b"\x1b[?1000l" in output and b"\x1b[?25h" in output


check("terminal restored: leave alt screen, mouse off, cursor shown", restored(tail))
time.sleep(0.3)
check("no process left in the session after exit", not t.session_processes(), str(t.session_processes()))

t2 = Tui(ARGV, cols=80, rows=24)
t2.send("3", 0.5)
os.write(t2.fd, b"q")
code = t2.wait_exit()
check("'q' exits from Services (exit 0)", code == 0, f"exit={code}")

# Signals quit like `q`, restore the terminal, stop the collectors, and then end
# the process with the signal itself.
for sig in (signal.SIGTERM, signal.SIGHUP):
    ts = Tui(ARGV, cols=80, rows=24)
    ts.send("4", 0.8)
    ts.all = b""
    os.kill(ts.pid, sig)
    code = ts.wait_exit(1.0)
    time.sleep(0.3)
    left = ts.session_processes()
    check(f"{sig.name} ends tuxctl by the signal within 1 s", code == -sig, f"exit={code}")
    check(f"{sig.name} restores the terminal", restored(ts.all[-400:]))
    check(f"{sig.name} leaves no process in the session", not left, str(left))

for name, ok, detail in results:
    print(f"{'PASS' if ok else 'FAIL'}  {name}{('  ' + detail) if detail and not ok else ''}")
passed = sum(ok for _, ok, _ in results)
print(f"{passed}/{len(results)} passed")
sys.exit(0 if passed == len(results) else 1)
