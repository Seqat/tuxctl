"""Minimal pty driver shared by the tuxctl development scripts (stdlib only)."""

import fcntl
import os
import pty
import select
import signal
import struct
import termios
import time


class Tui:
    """Runs tuxctl in a pseudo-terminal and drains its output."""

    def __init__(self, argv, cols=160, rows=50, env=None, settle=2.0):
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

    def pump(self, seconds):
        """Reads output for `seconds`; returns early if the child has exited."""
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], max(0, end - time.time()))
            if ready:
                try:
                    data = os.read(self.fd, 65536)
                except OSError:
                    return
                self.buf += data
                self.all += data

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
        """Returns the exit code, or None if the child is still running."""
        end = time.time() + timeout
        while time.time() < end:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.pump(0.1)
                return os.waitstatus_to_exitcode(status)
            self.pump(0.05)
        return None

    def quit(self):
        if self.alive():
            os.write(self.fd, b"q")
            self.wait_exit()

    def cpu_ticks(self):
        fields = open(f"/proc/{self.pid}/stat").read().rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])

    def rss_kib(self):
        for line in open(f"/proc/{self.pid}/status"):
            if line.startswith("VmRSS"):
                return int(line.split()[1])
        return 0
