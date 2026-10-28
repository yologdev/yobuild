#!/usr/bin/env python3
"""`yobuild run` with the terminal UI, on a real (pseudo-)terminal.

The terminal UI ends the session when it quits. Checked here: a typed task
runs; Ctrl+C when idle quits with exit code 0 (the last run completed); and
a terminal UI whose runtime dies ends the session with an error (exit 1)
rather than leaving the agent waiting with nobody there. The MCP server
runs in a process group of its own and, once its input closes, leaves a
`sleep` behind (as a server that ignores its closed input would): however
the session ends — quit, the UI dying, or a SIGINT to yobuild's group while
it starts — the server and its `sleep` are gone once yobuild has exited.

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
# The echo server, then a `sleep` that outlives its closed input.
lingering = os.path.join(project, "lingering-server.sh")
with open(lingering, "w") as f:
    f.write(f'#!/bin/sh\n"{fixture}/server.sh" "$@"\nexec sleep 300\n')
os.chmod(lingering, 0o755)
with open(os.path.join(project, "agent.toml"), "w") as f:
    f.write(
        '[agent]\nname = "echoer"\nmodel = "deepseek/deepseek-flash"\n\n'
        f'[[mcp]]\nnpm = "{lingering}"\nargs = ["first"]\n\n'
        '[builtins]\ngate = "off"\n\n[frontend]\nterminal = true\n'
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


def alive(p):
    out = subprocess.run(["ps", "-o", "stat=", "-p", str(p)], capture_output=True, text=True).stdout
    return out.strip() != "" and not out.strip().startswith("Z")


def server_pids(pid):
    """The server's processes under yobuild (its script and what it ran)."""
    return [p for p, cmd in tree(pid) if "lingering-server.sh" in cmd or "server.sh" in cmd]


def server_gone(pids, what):
    """Each of `pids` ended within 5 s of yobuild's exit."""
    end = time.time() + 5
    while time.time() < end and any(alive(p) for p in pids):
        time.sleep(0.1)
    left = [p for p in pids if alive(p)]
    if left:
        failures.append(f"{what}: the MCP server outlived yobuild ({left})")
        for p in left:
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass


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
    servers = server_pids(pid)
    if not servers:
        failures.append("no MCP server found under yobuild")
    os.write(fd, b"echo hi\r")
    if not screen.read_until(b"done", 30):
        failures.append("the typed task did not run")
    time.sleep(0.5)
    os.write(fd, b"\x03")
    code = exit_code(pid, screen, 30)
    if code != 0:
        failures.append(f"quitting from the terminal UI exited {code}, not 0")
    server_gone(servers, "quit")
kill_all(pid)

# 2. The terminal UI's runtime dies: the session ends with an error.
pid, fd = start()
screen = Screen(fd)
if not screen.read_until(b"terminal frontend", 90):
    failures.append("the terminal UI never drew its header (second start)")
else:
    time.sleep(1)
    servers = server_pids(pid)
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
    server_gone(servers, "the UI dying")
kill_all(pid)

# 3. SIGINT to yobuild's group while it starts (a Ctrl+C before the
# terminal UI has the keyboard): it stops, and the server with it.
pid, fd = start()
screen = Screen(fd)
servers, end = [], time.time() + 60
while not servers and time.time() < end and not screen.closed:
    servers = server_pids(pid)
    screen.read_until(b"\0never\0", 0.05)
if not servers:
    failures.append("no MCP server found under yobuild (third start)")
else:
    os.killpg(pid, signal.SIGINT)
    code = exit_code(pid, screen, 30)
    if code is None:
        failures.append("a SIGINT while starting did not end yobuild")
    server_gone(servers, "a SIGINT while starting")
kill_all(pid)

if failures:
    print("FAILED:", "; ".join(failures))
    print("screen (last 2000 bytes):", screen.text[-2000:].decode("utf-8", "replace"))
    sys.exit(1)
print("ok: a typed task ran, Ctrl+C quit with 0, a dead terminal UI ended the run with an error, and the MCP server never outlived yobuild")
