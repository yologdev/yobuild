#!/usr/bin/env python3
"""The terminal UI on a real (pseudo-)terminal, given text that holds
terminal escape sequences and deceptive links (design §5, "Untrusted text in
the terminal UI"): in its task (the prompt line), in a question it asks
(`ask_owner`: title, message, options, and the tool line's arguments) and in
its answer (Markdown). None reaches the terminal as a sequence.

Usage (Unix): terminal_text_pty.py <path to yobuild>
Runs tests/fixtures/hostile-text twice, with and without PI_HYPERLINKS=1
(pi-tui's hyperlink override, which the UI's own setting outranks), in a
120-column window. pi-tui writes ESC[2J itself on a redraw, so the screen
clear is checked by the answer's text: each sequence removed whole leaves
the two spaces around it.
"""
import fcntl
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time

yobuild = sys.argv[1]
fixture = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "hostile-text")
TASK = "go \x1b]0;PROMPTTITLE\x07 ‮back"


def run(hyperlinks):
    env = dict(os.environ, TERM="xterm-256color")
    env.pop("PI_HYPERLINKS", None)
    if hyperlinks:
        env["PI_HYPERLINKS"] = "1"
    pid, fd = pty.fork()
    if pid == 0:
        try:
            os.execve(
                yobuild,
                [yobuild, "run", os.path.join(fixture, "agent.toml"), "--scripted", os.path.join(fixture, "script.json"), TASK],
                env,
            )
        finally:
            os._exit(127)
    # A wide window: nothing checked here wraps.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    screen = b""

    def read(seconds, until=None):
        nonlocal screen
        end = time.time() + seconds
        while time.time() < end and not (until and until in screen):
            ready, _, _ = select.select([fd], [], [], 0.2)
            if ready:
                try:
                    screen += os.read(fd, 65536)
                except OSError:
                    return

    read(120, b"END-MARK")
    read(1.5)  # the rest of the frame
    # Ctrl+C quits when idle (the question left open).
    try:
        os.write(fd, b"\x03")
    except OSError:
        pass
    status = None
    deadline = time.time() + 20
    while time.time() < deadline:
        done, raw = os.waitpid(pid, os.WNOHANG)
        if done:
            status = os.waitstatus_to_exitcode(raw)
            break
        read(0.2)
    if status is None:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    return screen, status


failures = []
for hyperlinks in (False, True):
    screen, status = run(hyperlinks)
    label = f"PI_HYPERLINKS={'1' if hyperlinks else 'unset'}"
    checks = {
        "the answer was drawn": b"END-MARK" in screen,
        # Each sequence removed whole: the screen clear among them.
        "the answer's text, each sequence gone whole": b"BEGIN-MARK title  clipboard  query  clear  c1 2J0;C1 bidi evil" in screen,
        "no window title, from the answer, the task or the question": not re.search(rb"\x1b\][0-2];", screen),
        "nothing of the answer's title, clipboard or query": all(w not in screen for w in (b"PWNEDTITLE", b"cHduZWQ", b"21t")),
        "nothing of the task's title": b"PROMPTTITLE" not in screen,
        # The UI's own colours sit between its marker and the text.
        "the task's line, cleaned": "›".encode() in screen and b"\x1b[0mgo  back" in screen,
        "the question's title and message, cleaned": b"Q  end\x1b[0m" in screen and "— W  why".encode() in screen,
        "no clipboard write": b"\x1b]52;" not in screen,
        "no C1 controls": b"\xc2\x9b" not in screen and b"\xc2\x9d" not in screen,
        "no bidi override": "‮".encode() not in screen,
        # pi-tui ends each line with an empty `ESC]8;;` (no link): none has a target.
        "no terminal hyperlink to anywhere": re.search(rb"\x1b\]8;[^;\x07\x1b]*;[^\x07\x1b]", screen) is None,
        "the answer read as Markdown (no link source left)": all(w not in screen for w in (b"](javascript:", b"](https://evil", b"[click]")),
        "the javascript: target shown as text": b"(javascript:alert(1))" in screen,
        "a link's real target shown after its text": b"(https://evil.example)" in screen,
        "an autolink's target shown": b"(http://www.evil.example)" in screen,
        "Ctrl+C quit with 0": status == 0,
    }
    for name, ok in checks.items():
        print(f"{label}: {'ok ' if ok else 'FAIL'} {name}")
        if not ok:
            failures.append(f"{label}: {name}")
    if any(f.startswith(label) for f in failures):
        print(f"{label}: exit {status}; screen tail: {screen[-3000:]!r}")

if failures:
    print("terminal text check failed:", "; ".join(failures))
    sys.exit(1)
print("terminal text check passed")
