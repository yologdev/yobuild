#!/usr/bin/env python3
"""The terminal UI on a real (pseudo-)terminal, given an answer that holds
terminal escape sequences and deceptive links (design §5, "Untrusted text in
the terminal UI"): none reaches the terminal as a sequence.

Usage (Unix): terminal_text_pty.py <path to yobuild>
Runs tests/fixtures/hostile-text twice, with and without PI_HYPERLINKS=1
(pi-tui's hyperlink override, which the UI's own setting outranks).
pi-tui writes ESC[2J itself on a redraw, so the checks are the sentinels'.
"""
import os
import pty
import re
import select
import signal
import sys
import time

yobuild = sys.argv[1]
fixture = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "hostile-text")


def run(hyperlinks):
    env = dict(os.environ, TERM="xterm-256color")
    env.pop("PI_HYPERLINKS", None)
    if hyperlinks:
        env["PI_HYPERLINKS"] = "1"
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(
            yobuild,
            [yobuild, "run", os.path.join(fixture, "agent.toml"), "--scripted", os.path.join(fixture, "script.json"), "go"],
            env,
        )
    screen = b""
    end = time.time() + 120
    while time.time() < end and b"END-MARK" not in screen:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                screen += os.read(fd, 65536)
            except OSError:
                break
    # A moment more for the rest of the frame, then Ctrl+C quits when idle.
    settle = time.time() + 1.5
    while time.time() < settle:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                screen += os.read(fd, 65536)
            except OSError:
                break
    try:
        os.write(fd, b"\x03")
    except OSError:
        pass
    deadline = time.time() + 20
    while time.time() < deadline:
        done, _ = os.waitpid(pid, os.WNOHANG)
        if done:
            break
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                screen += os.read(fd, 65536)
            except OSError:
                pass
    else:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    return screen


failures = []
for hyperlinks in (False, True):
    screen = run(hyperlinks)
    label = f"PI_HYPERLINKS={'1' if hyperlinks else 'unset'}"
    checks = {
        "the answer was drawn": b"END-MARK" in screen,
        "no window title set by the answer": b"\x1b]0;PWNEDTITLE" not in screen,
        "no clipboard write": b"\x1b]52;" not in screen,
        "no title report query": b"\x1b[21t" not in screen,
        "no C1 controls": b"\xc2\x9b" not in screen and b"\xc2\x9d" not in screen,
        "no bidi override": "\u202e".encode() not in screen,
        # pi-tui ends each line with an empty `ESC]8;;` (no link): none has a target.
        "no terminal hyperlink to anywhere": re.search(rb"\x1b\]8;[^;\x07\x1b]*;[^\x07\x1b]", screen) is None,
        "the javascript: target shown as text": b"(javascript:alert(1))" in screen,
        "a link's real target shown after its text": b"(https://evil.example)" in screen,
    }
    for name, ok in checks.items():
        print(f"{label}: {'ok ' if ok else 'FAIL'} {name}")
        if not ok:
            failures.append(f"{label}: {name}")
    if failures:
        tail = screen[-3000:]
        print(f"{label}: screen tail: {tail!r}")

if failures:
    print("terminal text check failed:", "; ".join(failures))
    sys.exit(1)
print("terminal text check passed")
