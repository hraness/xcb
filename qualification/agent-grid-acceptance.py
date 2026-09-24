#!/usr/bin/env python3
"""Exercise the synthetic agent_grid_fixture binary in a real, isolated PTY.

This is operator evidence, not provider qualification. No packages, accounts,
network, daemon, or existing xcb state are needed. Captures and the intent log
are retained in a fresh private directory under --evidence-dir.
"""
import argparse
import codecs
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import tempfile
import termios
import time
import unicodedata


LIMIT = 8 * 1024 * 1024
F6 = b"\x1b[17~"
ESC = b"\x1b"
UP, DOWN, RIGHT = b"\x1b[A", b"\x1b[B", b"\x1b[C"
HOME, END = b"\x1b[H", b"\x1b[F"
PAGEUP, PAGEDOWN = b"\x1b[5~", b"\x1b[6~"


class Screen:
    """Small VT screen for crossterm's cursor, erase, SGR and resize output.

    Assertions inspect the final cells, never concatenate redraws into an
    imaginary screen. Wide Unicode cells and split UTF-8/CSI writes are kept.
    Unsupported printable-affecting CSI fails instead of silently misreading.
    """
    def __init__(self, rows, cols):
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.fg, self.bg = None, None
        self.resize(rows, cols)

    def resize(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.cells = [[(" ", None, None) for _ in range(cols)] for _ in range(rows)]
        self.x = self.y = 0

    def text(self):
        return "\n".join("".join(cell[0] for cell in row) for row in self.cells)

    def find(self, text):
        for row, cells in enumerate(self.cells):
            line = "".join(cell[0] for cell in cells)
            column = line.find(text)
            if column >= 0:
                return column, row
        return None

    def foreground(self, text):
        point = self.find(text)
        if point is None:
            raise AssertionError(f"text absent when checking its color: {text}")
        return self.cells[point[1]][point[0]][1]

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending[0] == "\x1b":
                if len(self.pending) < 2:
                    return
                if self.pending[1] == "[":
                    match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", self.pending)
                    if not match:
                        return
                    self.csi(*match.groups())
                    self.pending = self.pending[match.end():]
                    continue
                if self.pending[1] == "]":
                    match = re.search(r"\x07|\x1b\\", self.pending[2:])
                    if not match:
                        return
                    self.pending = self.pending[2 + match.end():]
                    continue
                if self.pending[1] in "()":
                    if len(self.pending) < 3:
                        return
                    self.pending = self.pending[3:]
                    continue
                raise AssertionError(f"unsupported terminal escape {self.pending[:12]!r}")
            char, self.pending = self.pending[0], self.pending[1:]
            if char == "\r":
                self.x = 0
            elif char == "\n":
                self.y = min(self.rows - 1, self.y + 1)
            elif char == "\b":
                self.x = max(0, self.x - 1)
            elif char >= " " and char != "\x7f":
                if unicodedata.combining(char):
                    if self.x:
                        old = self.cells[self.y][self.x - 1]
                        self.cells[self.y][self.x - 1] = (old[0] + char, old[1], old[2])
                    continue
                width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                if self.x >= self.cols:
                    self.x = 0
                    self.y = min(self.rows - 1, self.y + 1)
                self.cells[self.y][self.x] = (char, self.fg, self.bg)
                if width == 2 and self.x + 1 < self.cols:
                    self.cells[self.y][self.x + 1] = ("", self.fg, self.bg)
                self.x += width

    def csi(self, source, intermediate, final):
        private = source[:1] in ("?", ">", "<", "=")
        if private or final in ("h", "l", "u", "n", "c", "t", "q"):
            return
        values = [int(value) if value else 0 for value in source.split(";")]
        first = values[0] or 1
        if final in ("H", "f"):
            self.y = min(self.rows - 1, first - 1)
            self.x = min(self.cols - 1, (values[1] if len(values) > 1 and values[1] else 1) - 1)
        elif final == "G": self.x = min(self.cols - 1, first - 1)
        elif final == "d": self.y = min(self.rows - 1, first - 1)
        elif final == "A": self.y = max(0, self.y - first)
        elif final == "B": self.y = min(self.rows - 1, self.y + first)
        elif final == "C": self.x = min(self.cols - 1, self.x + first)
        elif final == "D": self.x = max(0, self.x - first)
        elif final == "E": self.y, self.x = min(self.rows - 1, self.y + first), 0
        elif final == "F": self.y, self.x = max(0, self.y - first), 0
        elif final == "J":
            for y in range(self.rows):
                for x in range(self.cols):
                    if values[0] in (2, 3) or (values[0] == 0 and (y, x) >= (self.y, self.x)) or (values[0] == 1 and (y, x) <= (self.y, self.x)):
                        self.cells[y][x] = (" ", self.fg, self.bg)
        elif final in ("K", "X"):
            start = 0 if final == "K" and values[0] in (1, 2) else self.x
            end = self.x + first if final == "X" else (self.x + 1 if values[0] == 1 else self.cols)
            for x in range(start, min(end, self.cols)):
                self.cells[self.y][x] = (" ", self.fg, self.bg)
        elif final == "m":
            index = 0
            while index < len(values):
                value = values[index]
                if value == 0: self.fg, self.bg = None, None
                elif value == 39: self.fg = None
                elif value == 49: self.bg = None
                elif 30 <= value <= 37 or 90 <= value <= 97: self.fg = value
                elif 40 <= value <= 47 or 100 <= value <= 107: self.bg = value
                elif value in (38, 48):
                    size = 2 if values[index + 1] == 5 else 4
                    color = tuple(values[index + 1:index + size + 1])
                    if value == 38: self.fg = color
                    else: self.bg = color
                    index += size
                index += 1
        else:
            raise AssertionError(f"unsupported terminal CSI {source}{intermediate}{final}")


def self_test():
    screen = Screen(4, 12)
    for data in [b"\x1b[2J\x1b[2;3H\x1b[38;5;", b"11mA", "東京".encode()[:2], "東京".encode()[2:], b"\x1b[0mZ"]:
        screen.feed(data)
    assert screen.find("A東京Z") == (2, 1)
    assert screen.cells[1][2][1] == (5, 11)
    assert screen.cells[1][7][1] is None
    screen.feed(b"\x1b[2;3H\x1b[2X")
    assert screen.cells[1][2][0] == " "
    screen.feed(b"\x1b[1;1Hstart\x1b[1;2H\x1b[K")
    assert screen.text().splitlines()[0].strip() == "s"
    screen.feed(b"\x1b[?1049h\x1b[>1u\x1b[?25l\x1b]0;title\x07")
    screen.resize(2, 7)
    assert screen.text() == "       \n       "
    print(json.dumps({"self_test": "passed", "checks": 7}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?")
    parser.add_argument("--evidence-dir", type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not args.binary:
        parser.error("the agent_grid_fixture binary is required")
    binary = args.binary.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="xcb-agent-grid-", dir=args.evidence_dir.resolve(strict=True)))
    for name in ("home", "state", "coord", "workspace", "tmp"):
        (root / name).mkdir(mode=0o700)
    env = {"HOME": str(root / "home"), "TMPDIR": str(root / "tmp"), "PATH": "/usr/bin:/bin",
           "TERM": "xterm-256color", "LANG": "en_US.UTF-8", "XCB_STATE": str(root / "state"),
           "XCB_COORDINATION_ROOT": str(root / "coord"), "HRANESS_SUPPORT_AUDIENCE": "off"}
    events_path = root / "intents.jsonl"
    control_path = root / "control.json"
    control_path.write_text('{"revision": 0}\n')
    control_path.chmod(0o600)
    update_revision = 0
    checks, snapshots, errors = [], [], []
    screen, capture = Screen(40, 132), bytearray()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root / "workspace")
        os.execve(str(binary), [str(binary), "--events", str(events_path), "--control", str(control_path)], env)
    status = None
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 132, 0, 0))
    os.set_blocking(fd, False)

    def check(name, condition, **detail):
        checks.append({"check": name, "passed": bool(condition), **detail})
        if not condition:
            raise AssertionError(name)

    def drain(seconds=.25):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([fd], [], [], min(.05, max(0, deadline - time.monotonic())))
            if ready:
                try: data = os.read(fd, 65536)
                except OSError: return
                if not data: return
                if len(capture) + len(data) > LIMIT:
                    raise RuntimeError("terminal output exceeded capture limit")
                capture.extend(data)
                screen.feed(data)

    def send(data, wait=.3):
        pending, deadline = memoryview(data), time.monotonic() + 5
        while pending:
            if time.monotonic() > deadline:
                raise RuntimeError("PTY input deadline exceeded")
            _, writable, _ = select.select([], [fd], [], .05)
            if writable:
                try: pending = pending[os.write(fd, pending):]
                except BlockingIOError: pass
        drain(wait)

    def snapshot(name):
        (root / f"{name}.txt").write_text(screen.text())
        snapshots.append({"name": name, "rows": screen.rows, "columns": screen.cols, "capture_bytes": len(capture)})
        return screen.text()

    def log():
        return [json.loads(line) for line in events_path.read_text().splitlines() if line]

    def no_intents(name):
        rows = log()
        check(name, all(row["kind"] in ("fixture", "fixture_update") for row in rows), events=rows)

    def visible_agents():
        return [int(number) for number in re.findall(r"\bAgent (\d\d) (?:Workspace work|Other workspace)", "\n".join(screen.text().splitlines()[:screen.rows // 2]))]

    def fixture_update(action, **details):
        nonlocal update_revision
        update_revision += 1
        next_path = root / "control.next"
        next_path.write_text(json.dumps({"revision": update_revision, "action": action, **details}))
        next_path.chmod(0o600)
        next_path.replace(control_path)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            drain(.1)
            if any(row.get("kind") == "fixture_update" and row.get("revision") == update_revision for row in log()):
                drain(.3)
                return
        raise AssertionError(f"synthetic update {update_revision} was not acknowledged")

    def command(text):
        send(b"\x01\x0b" + text.encode() + b"\r")

    def mouse(button, x, y):
        send(f"\x1b[<{button};{x + 1};{y + 1}M".encode())

    def resize(rows, cols):
        screen.resize(rows, cols)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        os.kill(pid, signal.SIGWINCH)
        drain(.6)

    try:
        deadline = time.monotonic() + 12
        while (screen.find("Agent 01") is None or screen.find("›") is None) and time.monotonic() < deadline:
            drain(.1)
        initial = snapshot("01-initial")
        expected = [3, 4, 5, 6, 7, 8, 13, 1, 2, 12, 15, 18]
        check("real terminal shows session grid and composer", "Agent 01" in initial and "›" in initial)
        check("default prioritizes attention then active sessions across workspaces", visible_agents() == expected, observed=visible_agents())
        check("default includes sessions from another workspace", "Agent 18" in initial and "Other workspace" in initial)
        check("names models response previews and activity visible", all(value in initial for value in ("codex/fixture", "claude/fixture", "RESPONSE-01", "thinking", "working")))
        check("attention states have readable labels", all(value in initial for value in ("needs answer", "needs approval", "needs action", "usage limit", "failed", "needs recovery")))
        colors = {number: screen.foreground(f"RESPONSE-{number:02}") for number in (1, 2, 3, 7)}
        check("response category independent of live working state", colors[2] != colors[1])
        check("overflow does not swallow main chat", "TRANSCRIPT-80" in initial and "Agent 17" not in initial)
        positions = [index for index, line in enumerate(initial.splitlines()) if re.search(r"Agent \d\d|RESPONSE-\d\d", line)]
        check("overview content stays within half viewport", positions and max(positions) < screen.rows // 2)
        fixture_update("recency")
        snapshot("02-recency-update")
        check("recency and incoming row order changes do not reshuffle cards", visible_agents() == expected, observed=visible_agents())
        send(b"draft survives")
        send(F6 + HOME)
        fixture_update("attention", agent=18)
        held = snapshot("03-focused-attention-update")
        check("new attention updates status without moving focused cards", visible_agents() == expected and "ATTENTION-18" in held, observed=visible_agents())
        check("live updates preserve main composer draft", "draft survives" in held)
        send(ESC)
        promoted = snapshot("04-attention-promoted")
        check("returning to chat promotes attention while preserving peer order", visible_agents() == [3, 4, 5, 6, 7, 8, 13, 18, 1, 2, 12, 15], observed=visible_agents())
        fixture_update("restore")
        send(F6 + HOME)
        send(RIGHT + DOWN + PAGEDOWN)
        moved = snapshot("05-keyboard-page")
        check("grid keyboard navigation pages overflow", "Agent 03" not in moved and "draft survives" in moved)
        send(PAGEUP + HOME)
        send(b"\r")
        send(b"\x01")
        reference = snapshot("06-keyboard-reference")
        check("Enter inserts selected reference into existing draft", "draft survives" in reference and "grid-agent-03" in reference)
        no_intents("reference selection neither sends nor navigates")
        send(b"\x01\x0b")
        send(F6 + END)
        last = snapshot("07-final-sessions")
        check("End reaches resting sessions from every workspace", all(f"Agent {number:02}" in last for number in (9, 10, 11, 14, 16, 17)))
        check("idle and cancelled states retain readable labels", "idle" in last and "cancelled" in last)
        colors.update({number: screen.foreground(f"RESPONSE-{number:02}") for number in (9, 10)})
        check("response categories use distinct terminal colors", len({str(value) for value in colors.values()}) == 5, colors=colors)
        check("completed preview stays green when session works again", colors[2] == colors[10])
        check("all eighteen sessions reachable with no workspace scope", set(expected) | set(visible_agents()) == set(range(1, 19)))
        send(HOME + b"2")
        active = snapshot("08-active-filter")
        check("active filter retains working and attention sessions", set(visible_agents()) == set(expected), observed=visible_agents())
        send(b"3")
        attention = snapshot("09-attention-filter")
        check("attention filter shows only sessions needing attention", visible_agents() == [3, 4, 5, 6, 7, 8, 13], observed=visible_agents())
        send(b"1" + ESC)
        send(b"filter draft survives")
        send(F6 + b"/Other workspace\r")
        filtered = snapshot("10-text-filter")
        check("slash filter matches sessions across workspaces", set(visible_agents()) == {17, 18}, observed=visible_agents())
        check("session filter preserves main composer", "filter draft survives" in filtered)
        # Esc clears the retained query, then returns focus to the composer.
        send(ESC + ESC)
        command("/overview clear")
        send(b"keyboard filter draft")
        send(F6 + b"\x06Agent 17\r")
        filtered = snapshot("11-control-f-filter")
        check("Ctrl-F filters without editing main composer", visible_agents() == [17] and "keyboard filter draft" in filtered, observed=visible_agents())
        send(ESC + ESC)
        command("/overview clear")
        send(b"back in chat")
        check("Escape returns typing to main composer", "back in chat" in snapshot("12-return-composer"))
        command("/overview hide")
        hidden = snapshot("13-hidden")
        check("overview hide reclaims main chat", not re.search(r"Agent \d\d", hidden) and "TRANSCRIPT-80" in hidden)
        command("/overview all")
        send(F6 + HOME + ESC)
        command("/mouse")
        check("mouse capture explicitly enabled", b"\x1b[?1006h" in capture)
        before = snapshot("14-before-grid-wheel")
        point = screen.find("Agent 03")
        check("first card has screen coordinates", point is not None)
        mouse(65, *point)
        after = snapshot("15-grid-wheel")
        check("wheel inside grid moves grid", before.splitlines()[:20] != after.splitlines()[:20])
        check("grid wheel leaves transcript fixed", re.findall(r"TRANSCRIPT-\d+", before) == re.findall(r"TRANSCRIPT-\d+", after))
        browsed_order = visible_agents()
        fixture_update("attention", agent=17)
        browsed = snapshot("16-browsed-attention-update")
        check("new attention does not move cards during scrolled browsing", visible_agents() == browsed_order, observed=visible_agents())
        grid_before = browsed.splitlines()[:20]
        mouse(64, 50, 28)
        after_chat = snapshot("17-chat-wheel")
        check("wheel below grid moves transcript", re.findall(r"TRANSCRIPT-\d+", browsed) != re.findall(r"TRANSCRIPT-\d+", after_chat))
        check("chat wheel leaves grid fixed", grid_before == after_chat.splitlines()[:20])
        point = next((screen.find(f"Agent {number:02}") for number in range(1, 19) if screen.find(f"Agent {number:02}")), None)
        check("scrolled card remains clickable", point is not None)
        title_line = screen.text().splitlines()[point[1]][point[0]:]
        selected_number = re.match(r"Agent (\d\d)", title_line).group(1)
        mouse(0, *point)
        send(b"\x01")
        clicked = snapshot("18-mouse-reference")
        check("click inserts clicked agent reference", f"grid-agent-{selected_number}" in clicked)
        no_intents("mouse and wheel perform no provider or navigation action")
        command("/help")
        snapshot("19-modal-open")
        mouse(0, *point)
        mouse(65, *point)
        send(F6)
        modal_after = snapshot("20-modal-shield")
        check("modal remains present over grid interactions", "shortcut" in modal_after.lower() or "help" in modal_after.lower())
        check("modal does not insert reference", "grid-agent-" not in modal_after)
        send(ESC)
        no_intents("modal shields underlying agent actions")
        resize(10, 32)
        tiny = snapshot("21-tiny-terminal")
        check("tiny terminal preserves composer", "›" in tiny)
        send(F6 + END + ESC)
        resize(40, 132)
        large = snapshot("22-resized-back")
        check("resize restores agents and composer", bool(re.search(r"Agent \d\d", large)) and "›" in large)
        send(F6 + HOME)
        send(b"\r")
        send(b"please summarize")
        no_intents("reference remains draft until explicit submission")
        send(b"\r")
        submitted = [row for row in log() if row["kind"] == "submit_to"]
        check("explicit submission uses original main chat", len(submitted) == 1 and submitted[0]["context"] == {"kind": "conversation", "id": "grid-main"}, events=submitted)
        check("submitted prompt retains reference and guidance", "grid-agent-03" in submitted[0]["text"] and "please summarize" in submitted[0]["text"])
        check("selection never changes session or sends habitat controls", not any(row["kind"] in ("conversation", "resume", "habitat", "submit", "other") for row in log()))
        command("/quit")
        deadline = time.monotonic() + 5
        while status is None and time.monotonic() < deadline:
            child, code = os.waitpid(pid, os.WNOHANG)
            if child: status = code
            else: drain(.1)
        check("fixture exits cleanly", status is not None and os.waitstatus_to_exitcode(status) == 0)
        check("terminal capture restored on exit", b"\x1b[?1049l" in capture and b"\x1b[?1006l" in capture)
    except Exception as error:
        errors.append(f"{type(error).__name__}: {error}")
        snapshot("failure")
    finally:
        # Signal only the exact child we created, never discover or kill a
        # provider/session/process group. Join before releasing its PTY.
        if status is None:
            child, code = os.waitpid(pid, os.WNOHANG)
            if child:
                status = code
            else:
                os.kill(pid, signal.SIGTERM)
                deadline = time.monotonic() + 3
                while status is None and time.monotonic() < deadline:
                    child, code = os.waitpid(pid, os.WNOHANG)
                    if child: status = code
                    else: time.sleep(.05)
                if status is None:
                    os.kill(pid, signal.SIGKILL)
                    _, status = os.waitpid(pid, 0)
        os.close(fd)
        (root / "terminal.raw").write_bytes(capture)
        receipt = {"schema": "xcb.agent-grid-acceptance.v2", "behavior": "all-sessions-stable-priority-filter", "passed": not errors,
                   "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                   "checks": checks, "errors": errors, "snapshots": snapshots,
                   "capture_bytes": len(capture), "exit": os.waitstatus_to_exitcode(status),
                   "isolated": True, "provider_operations": False}
        (root / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"passed": receipt["passed"], "checks": len(checks), "errors": errors, "evidence": str(root)}))
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
