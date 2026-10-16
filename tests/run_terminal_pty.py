#!/usr/bin/env python3
"""`yobuild run` with the terminal UI, on a real (pseudo-)terminal.

The terminal UI ends the session when it quits. Checked here: a typed task
runs; Ctrl+C when idle quits with exit code 0 (the last run completed); and
a terminal UI whose runtime dies ends the session with an error (exit 1)
rather than leaving the agent waiting with nobody there.

Usage: run_terminal_pty.py <path to the built yobuild binary> (Unix).
"""
import os
import pty
import select
import signal
import subprocess
import sys
import tempfile
import time

exe = os.path.abspath(sys.argv[1])
fixture = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "echo")
project = tempfile.mkdtemp()
with open(os.path.join(project, "agent.toml"), "w") as f:
    f.write(
        '[agent]\nname = "echoer"\nmodel = "deepseek/deepseek-flash"\n\n'
        f'[[mcp]]\nnpm = "{fixture}/server.sh"\nargs = ["first"]\n\n'
        '[parts]\ngate = "off"\n\n[frontend]\nterminal = true\n'
    )


def start():
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(project)
        os.execv(
            exe,
            [exe, "run", "agent.toml", "--scripted", os.path.join(fixture, "script.json")],
        )
    return pid, fd


class Screen:
    def __init__(self, fd):
        self.fd, self.text, self.closed = fd, b"", False

    def read_until(self, needle, seconds):
        end = time.time() + seconds
        while time.time() < end and not self.closed:
            if needle in self.text:
                return True
            ready, _, _ = select.select([self.fd], [], [], 0.2)
            if ready:
                try:
                    self.text += os.read(self.fd, 65536)
                except OSError:
                    self.closed = True
        return needle in self.text


def exit_code(pid, screen, seconds):
    """Its exit code, reading the terminal meanwhile (a child writing to a
    full pty would block)."""
    end = time.time() + seconds
    while time.time() < end:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            return os.waitstatus_to_exitcode(status)
        screen.read_until(b"\0never\0", 0.2)
    return None


def tree(root):
    out = subprocess.run(
        ["ps", "-A", "-o", "pid=,ppid=,command="], capture_output=True, text=True
    ).stdout
    rows = [line.split(None, 2) for line in out.splitlines() if line.strip()]
    pids, grew = {root}, True
    while grew:
        grew = False
        for row in rows:
            if int(row[1]) in pids and int(row[0]) not in pids:
                pids.add(int(row[0]))
                grew = True
    return [(int(r[0]), r[2] if len(r) > 2 else "") for r in rows if int(r[0]) in pids and int(r[0]) != root]


def kill_all(pid):
    for p, _ in tree(pid) + [(pid, "")]:
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass


failures = []

# 1. A typed task runs; Ctrl+C when idle quits; exit 0.
pid, fd = start()
screen = Screen(fd)
if not screen.read_until(b"terminal frontend", 90):
    failures.append("the terminal UI never drew its header")
else:
    time.sleep(1)
    os.write(fd, b"echo hi\r")
    if not screen.read_until(b"done", 30):
        failures.append("the typed task did not run")
    time.sleep(0.5)
    os.write(fd, b"\x03")
    code = exit_code(pid, screen, 30)
    if code != 0:
        failures.append(f"quitting from the terminal UI exited {code}, not 0")
kill_all(pid)

# 2. The terminal UI's runtime dies: the session ends with an error.
pid, fd = start()
screen = Screen(fd)
if not screen.read_until(b"terminal frontend", 90):
    failures.append("the terminal UI never drew its header (second start)")
else:
    time.sleep(1)
    node = [p for p, cmd in tree(pid) if "node" in cmd and "server.sh" not in cmd]
    if not node:
        failures.append("no Node runtime found under yobuild")
    for p in node:
        os.kill(p, signal.SIGKILL)
    code = exit_code(pid, screen, 30)
    if code != 1:
        failures.append(f"a dead terminal UI left yobuild with exit {code}, not 1")
    elif b"the terminal UI stopped" not in screen.text:
        failures.append("yobuild did not say the terminal UI stopped")
kill_all(pid)

if failures:
    print("FAILED:", "; ".join(failures))
    print("screen (last 2000 bytes):", screen.text[-2000:].decode("utf-8", "replace"))
    sys.exit(1)
print("ok: a typed task ran, Ctrl+C quit with 0, and a dead terminal UI ended the run with an error")
