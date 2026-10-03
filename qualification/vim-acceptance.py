#!/usr/bin/env python3
"""Exercise the composer's Vim mode in a real, isolated PTY via ux_fixture.

This is operator evidence, not provider qualification. No packages, accounts,
network, daemon, or existing xcb state are needed. Captures and snapshots are
retained in a fresh private directory under --evidence-dir.
"""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import struct
import tempfile
import termios
import time

ESC = b"\x1b"
ENTER = b"\r"


def load_screen():
    source = Path(__file__).with_name("agent-grid-acceptance.py")
    spec = importlib.util.spec_from_file_location("grid_acceptance", source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Screen


Screen = load_screen()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?")
    parser.add_argument("--evidence-dir", type=Path, default=Path(tempfile.gettempdir()))
    args = parser.parse_args()
    if not args.binary:
        parser.error("the ux_fixture binary is required")
    binary = args.binary.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="xcb-vim-", dir=args.evidence_dir.resolve(strict=True)))
    for name in ("home", "state", "coord", "workspace", "tmp"):
        (root / name).mkdir(mode=0o700)
    env = {"HOME": str(root / "home"), "TMPDIR": str(root / "tmp"), "PATH": "/usr/bin:/bin",
           "TERM": "xterm-256color", "LANG": "en_US.UTF-8", "XCB_STATE": str(root / "state"),
           "XCB_COORDINATION_ROOT": str(root / "coord"), "HRANESS_SUPPORT_AUDIENCE": "off"}
    checks, snapshots = [], []
    screen, capture = Screen(40, 132), bytearray()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root / "workspace")
        os.execve(str(binary), [str(binary)], env)
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
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                capture.extend(data)
                screen.feed(data)

    def send(data, wait=.3):
        pending, deadline = memoryview(data), time.monotonic() + 5
        while pending:
            if time.monotonic() > deadline:
                raise RuntimeError("PTY input deadline exceeded")
            _, writable, _ = select.select([], [fd], [], .05)
            if writable:
                try:
                    pending = pending[os.write(fd, pending):]
                except BlockingIOError:
                    pass
        drain(wait)

    def snapshot(name):
        (root / f"{name}.txt").write_text(screen.text())
        snapshots.append({"name": name, "capture_bytes": len(capture)})
        return screen.text()

    def gutter():
        """The composer's prompt cell: '›' normally, 'I'/'N' under /vim."""
        for row in range(screen.rows - 1, -1, -1):
            cell = screen.cells[row][0][0]
            if cell in ("›", "I", "N"):
                return row, cell
        return None, None

    def composer_line(row):
        return "".join(cell[0] for cell in screen.cells[row])

    try:
        deadline = time.monotonic() + 12
        while gutter()[1] != "›" and time.monotonic() < deadline:
            drain(.1)
        row, cell = gutter()
        check("real terminal shows the default prompt gutter", cell == "›", gutter_row=row)
        send(b"/vim" + ENTER, .6)
        initial = snapshot("01-vim-on")
        check("/vim turns editing on with an insert indicator", "Vim editing on" in initial)
        row, cell = gutter()
        check("insert mode shows I in the gutter", cell == "I", gutter_row=row)
        send(b"one two three", .5)
        send(ESC, .5)
        row, cell = gutter()
        check("Esc enters normal mode with N in the gutter", cell == "N", gutter_row=row)
        send(b"0dw", .6)
        row, cell = gutter()
        check("dw deletes the first word", "two three" in composer_line(row),
              line=composer_line(row))
        send(b"w$", .4)
        send(b"X", .4)
        check("w $ X leave the draft edited at the word end", "two thre" in composer_line(row))
        send(b"u", .5)
        check("u restores the deleted character", "two three" in composer_line(row))
        send(ENTER, .8)
        sent = snapshot("02-submitted")
        row, cell = gutter()
        check("Enter submits from normal mode", "two three" in sent)
        check("submit returns to a clean insert indicator", cell == "I", gutter_row=row)
        # Esc, then `/` in normal mode drops back into insert holding the
        # slash, so the command menu opens and `/exit` quits normally.
        send(ESC, .4)
        row, cell = gutter()
        check("Esc re-enters normal mode after the send", cell == "N", gutter_row=row)
        send(b"/exit" + ENTER, 1.0)
        deadline = time.monotonic() + 8
        while status is None and time.monotonic() < deadline:
            drain(.2)
            done, status = os.waitpid(pid, os.WNOHANG)
            if done == 0:
                status = None
        if status is None:
            os.kill(pid, 9)
            raise AssertionError("fixture did not exit")
        check("fixture exits cleanly", os.waitstatus_to_exitcode(status) == 0)
    except BaseException as error:
        snapshot("99-failure")
        checks.append({"check": "run", "passed": False, "error": repr(error)})
    finally:
        if status is None:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass
    result = {"checks": checks, "snapshots": snapshots, "evidence": str(root)}
    (root / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if all(row["passed"] for row in checks) else 1)


if __name__ == "__main__":
    main()
