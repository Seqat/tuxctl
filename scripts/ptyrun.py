"""Minimal pty driver shared by the tuxctl development scripts (stdlib only)."""

import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import threading
import time

# Tab label drawn by the first frame at any width that shows full labels.
FIRST_FRAME_MARKER = b"Overview"


class Tui:
    """Runs tuxctl in a pseudo-terminal and drains its output."""

    def __init__(self, argv, cols=160, rows=50, env=None, settle=2.0):
        self.started = time.time()
        self.first_frame_ms = None
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ["TERM"] = "xterm-256color"
            os.environ.update(env or {})
            os.execv(argv[0], argv)
        self.buf = b""
        self.all = b""
        self.resize(cols, rows)
        self.pump(settle)

    def resize(self, cols, rows):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        os.kill(self.pid, signal.SIGWINCH)

    def _read(self, timeout):
        """Reads one chunk within `timeout`: b"" on timeout, None once the pty is closed."""
        ready, _, _ = select.select([self.fd], [], [], max(0, timeout))
        if not ready:
            return b""
        try:
            data = os.read(self.fd, 65536)
        except OSError:
            return None
        self.buf += data
        self.all += data
        if self.first_frame_ms is None and FIRST_FRAME_MARKER in self.all:
            self.first_frame_ms = (time.time() - self.started) * 1000
        return data

    def pump(self, seconds):
        """Reads output for `seconds`; returns early if the child has exited."""
        end = time.time() + seconds
        while time.time() < end:
            if self._read(end - time.time()) is None:
                return

    def wait_for(self, marker, timeout=2.0):
        """Milliseconds until `marker` appears in new output, or None on timeout."""
        start = time.time()
        seen = b""
        while time.time() - start < timeout:
            data = self._read(timeout - (time.time() - start))
            if data is None:
                return None
            seen += data
            if marker in seen:
                return (time.time() - start) * 1000
        return None

    def send(self, data, wait=0.35):
        self.buf = b""
        os.write(self.fd, data if isinstance(data, bytes) else data.encode())
        self.pump(wait)
        return self.buf

    def click(self, x, y, wait=0.35):
        return self.send(f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m", wait)

    def wheel(self, x, y, down=True):
        return self.send(f"\x1b[<{65 if down else 64};{x};{y}M", 0.2)

    def alive(self):
        pid, _ = os.waitpid(self.pid, os.WNOHANG)
        return pid == 0

    def wait_exit(self, timeout=3):
        """Returns the exit code (negative for a signal), or None if still running."""
        end = time.time() + timeout
        while time.time() < end:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.pump(0.1)
                return os.waitstatus_to_exitcode(status)
            self.pump(0.05)
        return None

    def quit(self):
        """Quits with `q`; returns the exit code (None if tuxctl did not exit)."""
        if self.alive():
            os.write(self.fd, b"q")
            return self.wait_exit()
        return None

    def cpu_ticks(self):
        fields = open(f"/proc/{self.pid}/stat").read().rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])

    def rss_kib(self):
        for line in open(f"/proc/{self.pid}/status"):
            if line.startswith("VmRSS"):
                return int(line.split()[1])
        return 0

    def session_processes(self):
        """(pid, comm) of other processes in tuxctl's session.

        `pty.fork` makes tuxctl a session leader; its children stay in the
        session even in their own process group or after being orphaned.
        """
        found = []
        for entry in os.listdir("/proc"):
            if not entry.isdigit() or int(entry) == self.pid:
                continue
            try:
                stat = open(f"/proc/{entry}/stat").read()
            except OSError:
                continue
            session = int(stat.rsplit(")", 1)[1].split()[3])
            if session == self.pid:
                found.append((int(entry), stat[stat.index("(") + 1 : stat.rindex(")")]))
        return found


class LogStorm:
    """Writes `rate` journal messages per second, tagged `tuxctl-storm`.

    One long-running `logger` reads the lines from stdin, so the rate is exact
    and the generator itself costs little CPU.
    """

    def __init__(self, rate=200):
        self.rate = rate
        self.sent = 0

    def __enter__(self):
        self.logger = subprocess.Popen(
            ["logger", "-t", "tuxctl-storm"], stdin=subprocess.PIPE, start_new_session=True
        )
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()
        return self

    def _run(self):
        start = time.monotonic()
        while not self.stop.is_set():
            delay = start + self.sent / self.rate - time.monotonic()
            if delay > 0 and self.stop.wait(delay):
                return
            try:
                self.logger.stdin.write(f"storm {self.sent}\n".encode())
                self.logger.stdin.flush()
            except OSError:
                return
            self.sent += 1

    def __exit__(self, *exc):
        self.stop.set()
        self.thread.join()
        self.logger.stdin.close()
        self.logger.wait()
