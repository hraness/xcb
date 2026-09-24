#!/usr/bin/env python3
"""Credential-free native inbox/program CLI/PTY acceptance; retain evidence files.

Run with an exact native binary, through the host scheduler where installed.
--evidence-dir selects an existing parent for a fresh private capture directory.
This verifies operator contracts, not live provider delivery or qualification.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import selectors
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


CAPTURE_LIMIT = 4 * 1024 * 1024
COMMAND_LIMIT = 256 * 1024
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--evidence-dir", type=Path, default=Path(tempfile.gettempdir()))
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    parent = args.evidence_dir.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="xcb-inbox-acceptance-", dir=parent)).resolve()
    paths = {name: root / name for name in ("state", "coord", "workspace", "home", "tmp")}
    for path in paths.values():
        path.mkdir(mode=0o700)
    # Do not pass provider keys, real HOME, provider configuration or real state.
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(paths["home"]),
           "TMPDIR": str(paths["tmp"]), "TERM": "xterm-256color", "LANG": "en_US.UTF-8",
           "XCB_STATE": str(paths["state"]), "XCB_COORDINATION_ROOT": str(paths["coord"]),
           "HRANESS_SUPPORT_AUDIENCE": "off"}
    base = [str(binary), "--state", str(paths["state"]), "--cwd", str(paths["workspace"])]
    results, errors = [], []
    daemon = None
    schedule = None
    started = time.time_ns() // 1_000_000
    sha = hashlib.sha256(binary.read_bytes()).hexdigest()

    def check(name, condition, **detail):
        results.append(dict(check=name, passed=bool(condition), **detail))
        if not condition:
            raise AssertionError(name)

    def command(*argv, ok=True):
        if daemon is not None:
            check("owned daemon remains active", daemon.poll() is None)
        log = root / "daemon.log"
        if log.exists():
            check("daemon log bounded", log.stat().st_size <= CAPTURE_LIMIT)
        proc = subprocess.Popen(base + list(argv), env=env, cwd=paths["workspace"],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        streams = {proc.stdout: bytearray(), proc.stderr: bytearray()}
        deadline = time.monotonic() + 25
        problem = None
        with selectors.DefaultSelector() as selector:
            for stream in streams:
                selector.register(stream, selectors.EVENT_READ)
            while selector.get_map():
                if time.monotonic() >= deadline:
                    problem = "command deadline exceeded"
                    break
                for key, _ in selector.select(min(.1, max(0, deadline - time.monotonic()))):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                    elif len(streams[key.fileobj]) + len(data) > COMMAND_LIMIT:
                        problem = "command output bound exceeded"
                        break
                    else:
                        streams[key.fileobj].extend(data)
                if problem:
                    break
        if problem and proc.poll() is None:
            proc.terminate()
        try:
            code = proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            # This is the exact child created above, never a discovered process.
            proc.kill()
            code = proc.wait(timeout=3)
            problem = problem or "command failed to join"
        stdout, stderr = (streams[stream].decode(errors="replace") for stream in (proc.stdout, proc.stderr))
        proc.stdout.close()
        proc.stderr.close()
        results.append(dict(command=list(argv), exit=code, stdout=stdout, stderr=stderr, problem=problem))
        if problem or (ok and code):
            raise RuntimeError(f"{argv}: {problem or stderr or code}")
        return code, stdout

    def value(*argv):
        return json.loads(command(*argv, "--json")[1])

    def task(task_id):
        return value("tasks", "show", task_id)

    def events(task_id):
        return value("inbox", "--task", task_id, "--limit", "256")

    def status(event):
        # Contract adapter: runtime InboxEvent currently uses these literal
        # status names. Unknown schemas must fail visibly, not be interpreted
        # as success. If that public contract changes, update this adapter.
        known = {"waiting", "queued", "prepared", "delivered", "held", "closed"}
        check("recognized inbox status", event.get("status") in known, status=event.get("status"))
        return event["status"]

    def complete(row, summary):
        current = task(row["id"])
        return value("backlog", "complete", row["id"], summary, "--revision", str(current["revision"]))

    def eventually(name, predicate):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if predicate():
                check(name, True)
                return
            time.sleep(.1)
        check(name, False)

    def start_daemon():
        nonlocal daemon
        with (root / "daemon.log").open("ab") as log:
            daemon = subprocess.Popen(base + ["managed-daemon"], env=env, cwd=paths["workspace"],
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        identity = paths["state"] / "managed" / "supervisor.identity.json"
        def registered():
            if not identity.exists() or daemon.poll() is not None:
                return False
            record = json.loads(identity.read_text())
            return record["pid"] == daemon.pid and record["sha256"] == sha and record["executable"] == str(binary)
        eventually("owned daemon exact identity", registered)

    def terminal(name, argv, actions, redraw_at=(), durable_at=None):
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(paths["workspace"])
            os.execve(str(binary), base + argv, env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 160, 0, 0))
        os.set_blocking(fd, False)
        capture = bytearray()
        segments = []
        wait_status = None

        def drain(seconds):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                readable, _, _ = select.select([fd], [], [], min(.1, max(0, until - time.monotonic())))
                if not readable:
                    continue
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    break
                if not data:
                    break
                if len(capture) + len(data) > CAPTURE_LIMIT:
                    raise RuntimeError("PTY capture bound exceeded")
                capture.extend(data)

        def reap(seconds):
            nonlocal wait_status
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                got, code = os.waitpid(pid, os.WNOHANG)
                if got:
                    wait_status = code
                    return True
                drain(.05)
            return False

        def write_all(data):
            pending = memoryview(data)
            deadline = time.monotonic() + 8
            while pending:
                if time.monotonic() >= deadline:
                    raise RuntimeError("PTY input deadline exceeded")
                _, writable, _ = select.select([], [fd], [], .1)
                if not writable:
                    drain(.01)
                    continue
                try:
                    written = os.write(fd, pending)
                except BlockingIOError:
                    continue
                if written <= 0:
                    raise RuntimeError("PTY input closed before complete write")
                pending = pending[written:]
                drain(.01)

        try:
            ready_by = time.monotonic() + 12
            while b"Ctrl-V" not in capture and time.monotonic() < ready_by:
                drain(.1)
            check(name + " composer ready", b"Ctrl-V" in capture)
            drain(.5)
            for index, (data, delay) in enumerate(actions):
                offset = len(capture)
                if data.startswith(b"\x1b[200~"):
                    check(name + " bracketed paste enabled " + str(index), b"\x1b[?2004h" in capture)
                write_all(data)
                drain(delay)
                if durable_at and index in durable_at:
                    # A slow host can still be consuming input after a fixed
                    # delay. Advance only after the exact operator write is
                    # durable, before opening a picker or selecting its row.
                    ready_by = time.monotonic() + 12
                    ready = durable_at[index]()
                    while not ready and time.monotonic() < ready_by:
                        drain(.1)
                        ready = durable_at[index]()
                    check(name + " durable action checkpoint " + str(index), ready)
                    drain(.2)
                if index in redraw_at:
                    # Ratatui emits changed cells, not whole strings. A raw
                    # ANSI-stripped delta can omit matching letters from the
                    # previous screen. Resize clears its cached frame; restore
                    # the exact viewport and inspect only that full redraw.
                    for columns in (159, 160):
                        offset = len(capture)
                        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 48, columns, 0, 0))
                        redraw_by = time.monotonic() + 8
                        while b"\x1b[2J" not in capture[offset:] and time.monotonic() < redraw_by:
                            drain(.1)
                        check(name + " full redraw checkpoint " + str(index) + " width " + str(columns), b"\x1b[2J" in capture[offset:])
                        # Observe the intermediate frame before restoring the
                        # viewport so a slow runner cannot coalesce both resizes.
                        drain(.2)
                segments.append(ANSI.sub("", capture[offset:].decode(errors="replace")))
            check(name + " exited without forced stop", reap(8))
            check(name + " exit success", os.waitstatus_to_exitcode(wait_status) == 0)
        finally:
            if wait_status is None:
                # Only the unreaped PTY child is signalled. The daemon has a
                # separate owned Popen identity and is joined below.
                os.kill(pid, signal.SIGTERM)
                if not reap(3):
                    os.kill(pid, signal.SIGKILL)
                    check(name + " forced child joined", reap(3))
            os.close(fd)
            (root / f"{name}.ansi").write_bytes(capture)
            screen = ANSI.sub("", capture.decode(errors="replace"))
            (root / f"{name}.txt").write_text(screen)
            (root / f"{name}.segments.json").write_text(json.dumps(segments, indent=2) + "\n")
        return screen, segments

    try:
        start_daemon()
        terminal("create-conversation", ["chat", "--new"], [(b"\x04", .3)])
        conversations = value("conversations")
        check("one isolated conversation", len(conversations) == 1)
        conversation = conversations[0]["id"]
        # Keep the exact daemon alive during PTY checks. The first occurrence
        # is a year away; this fixture never releases work or activates accounts.
        schedule = value("schedules", "add", conversation, "Acceptance keepalive; do not run", "--every", "31536000")[0]
        check("keepalive has no occurrence", schedule["last_task"] is None)
        check("isolated accounts empty", value("accounts")["accounts"] == [])
        target = value("backlog", "add", conversation, "Held inbox target")
        source = value("backlog", "add", conversation, "Reported source")
        alternate = value("backlog", "add", conversation, "Other target")
        event = value("steer", target["id"], "Preserve this deferred task", "--id", "acceptance_event_1")
        replay = value("steer", target["id"], event["text"], "--id", event["id"])
        check("stable steering replay", replay == event)
        check("changed steering conflicts", command("steer", target["id"], "Changed input", "--id", event["id"], ok=False)[0] != 0)
        check("changed target conflicts", command("steer", alternate["id"], event["text"], "--id", event["id"], ok=False)[0] != 0)
        value("steer", target["id"], "Second accepted input", "--id", "acceptance_event_2")
        value("steer", target["id"], "Third accepted input", "--id", "acceptance_event_3")
        watch = value("watch", target["id"], source["id"], "--id", "acceptance_watch_1")
        check("stable watch replay", value("watch", target["id"], source["id"], "--id", watch["id"]) == watch)
        check("changed watch conflicts", command("watch", alternate["id"], source["id"], "--id", watch["id"], ok=False)[0] != 0)
        complete(source, "CLI_SOURCE_REPORT_COMPLETE")
        rows = events(target["id"])
        check("watch produces exactly one report", len(rows) == 4 and sum("CLI_SOURCE_REPORT_COMPLETE" in row["text"] for row in rows) == 1)
        check("deferred guidance is held without receipt", all(status(row) == "held" and row.get("receipt") is None for row in rows))
        check("newest first sequences", [row["sequence"] for row in rows] == sorted((row["sequence"] for row in rows), reverse=True))
        page = value("inbox", "--task", target["id"], "--limit", "2")
        older = value("inbox", "--task", target["id"], "--before", str(page[-1]["sequence"]), "--limit", "2")
        check("exclusive pagination exactly covers known events", page + older == rows)
        filtered = value("inbox", "--conversation", conversation)
        check("conversation filter scopes every event", filtered and all(row["conversation"] == conversation for row in filtered))
        check("empty target filter", events(alternate["id"]) == [])
        check("mutually exclusive filters rejected", command("inbox", "--task", target["id"], "--conversation", conversation, ok=False)[0] != 0)
        for limit in ("0", "257"):
            check("out of bounds page rejected " + limit, command("inbox", "--limit", limit, ok=False)[0] != 0)
        saved = task(target["id"])
        check("inbox did not release or attempt deferred task", saved["deferred"] and saved["attempts"] == target["attempts"] and saved.get("session") is None)
        terminal("second-conversation", ["chat", "--new"], [(b"\x04", .3)])
        other_conversation = next(row["id"] for row in value("conversations") if row["id"] != conversation)
        other = value("backlog", "add", other_conversation, "Other project")
        value("steer", other["id"], "OTHER_PROJECT_GUIDANCE", "--id", "other_project_event")
        check("conversation filter excludes other project", all(row["conversation"] == conversation for row in value("inbox", "--conversation", conversation)))
        check("watch cannot cross project", command("watch", target["id"], other["id"], "--id", "cross_project_watch", ok=False)[0] != 0)
        closed = value("backlog", "add", conversation, "Target closes first")
        late_source = value("backlog", "add", conversation, "Late report source")
        value("watch", closed["id"], late_source["id"], "--id", "late_watch")
        complete(closed, "Target intentionally closed")
        complete(late_source, "LATE_REPORT_RETAINED")
        late = events(closed["id"])
        check("late report retained closed without delivery", len(late) == 1 and "LATE_REPORT_RETAINED" in late[0]["text"] and status(late[0]) == "closed" and late[0].get("receipt") is None)
        check("closed target not reopened", task(closed["id"])["state"] == "completed")
        check("closed target rejects new guidance", command("steer", closed["id"], "Must not reopen", "--id", "closed_guidance", ok=False)[0] != 0)
        ui_target = value("backlog", "add", conversation, "TUI inbox target")
        ui_source = value("backlog", "add", conversation, "TUI watch source")
        before_tasks = value("backlog", "--conversation", conversation)
        long_text = "UI_GUIDANCE_HEAD " + "keep the existing boundary; " * 130 + "UI_GUIDANCE_TAIL"
        # Use the same bracketed-paste protocol as a real terminal. Thousands
        # of synthetic key events otherwise take seconds on slower CI hosts.
        # Enter remains outside the paste, so it submits exactly once.
        def paste_submit(text):
            return b"\x1b[200~" + text.encode() + b"\x1b[201~\r"

        def ui_watch_saved():
            rows = events(ui_target["id"])
            return len(rows) == 1 and rows[0]["task"] == ui_target["id"] \
                and rows[0]["conversation"] == conversation and rows[0]["kind"] == "completion" \
                and rows[0]["status"] == "waiting" and ui_source["id"] in rows[0]["text"]

        def ui_guidance_saved():
            rows = events(ui_target["id"])
            return len(rows) == 2 and sum(row["text"] == long_text
                                        and row["task"] == ui_target["id"]
                                        and row["conversation"] == conversation
                                        and row["kind"] == "steering"
                                        and row["status"] == "held" for row in rows) == 1

        actions = [
            ((f"/watch {ui_target['id']} {ui_source['id']}\r").encode(), .7),
            (paste_submit(f"/steer {ui_target['id']} {long_text}"), .3),
            ((f"/inbox {ui_target['id']}\r").encode(), .5),
            (b"\r", .5), (b"\x1b[F", .5), (b"\x1b", .2),
            (b"/inbox all\r", .5), (b"\x1b", .2),
            (b"/inbox\r", .5), (b"\x1b", .2), (b"\x04", .3),
        ]
        _, segments = terminal("inbox-tui", ["chat", "--resume", conversation], actions,
                               redraw_at=(2, 3, 4, 6, 8),
                               durable_at={0: ui_watch_saved, 1: ui_guidance_saved})
        task_picker = re.sub(r"\s+", "", segments[2])
        all_picker = re.sub(r"\s+", "", segments[6])
        conversation_picker = re.sub(r"\s+", "", segments[8])
        inspector = re.sub(r"\s+", "", "".join(segments[3:5]))
        check("TUI exact task inbox rendered", "Taskinbox·recentdeliveryhistory" in task_picker)
        check("TUI all and conversation inbox rendered", "Allagents·inbox·recentdeliveryhistory" in all_picker and "Thisagent·inbox·recentdeliveryhistory" in conversation_picker)
        ui_rows = events(ui_target["id"])
        ui_guidance = [row for row in ui_rows if row["text"] == long_text]
        check("TUI steer targets exact task once", len(ui_guidance) == 1 and ui_guidance[0]["task"] == ui_target["id"])
        check("TUI watch reserves a waiting report", len(ui_rows) == 2 and sum(status(row) == "waiting" for row in ui_rows) == 1)
        check("TUI full event inspector identities", all(identifier in inspector for identifier in (ui_guidance[0]["id"], ui_target["id"], conversation)))
        check("TUI full inspector scrolls to content tail and receipt", "UI_GUIDANCE_TAIL" in inspector and "Deliveryreceipt" in inspector and "Nosettleddeliveryreceiptyet." in inspector)
        check("slash actions do not create ordinary tasks", len(value("backlog", "--conversation", conversation)) == len(before_tasks))
        complete(ui_source, "TUI_WATCH_REPORT_COMPLETE")
        ui_rows = events(ui_target["id"])
        check("TUI watch routes exact completion", len(ui_rows) == 2 and sum("TUI_WATCH_REPORT_COMPLETE" in row["text"] for row in ui_rows) == 1)
        check("TUI guidance keeps work deferred", task(ui_target["id"])["deferred"] and all(status(row) == "held" for row in ui_rows))
        check("no provider accounts activated", value("accounts")["accounts"] == [])
        check("no provider sessions created", value("sessions") == [])
        check("no fabricated attention", value("attention") == [])

        # Exercise real controller publication and a durable no-account child.
        # No fabricated provider output or direct database mutation is involved;
        # deterministic runtime tests cover completed-result replay separately.
        pure_file = root / "pure.algal.json"
        pure_file.write_text(json.dumps({
            "contract": "algal.organism.v1", "key": "organism:acceptance-pure",
            "cells": [{"id": "summary", "kind": "const", "outputs": {"out": {"type": "text", "value": "PURE_PROGRAM_COMPLETE"}}}],
            "edges": [], "interface": {"inputs": {}, "outputs": {"summary": {"cell": "summary", "port": "out"}}}
        }))
        pure = value("backlog", "program", conversation, str(pure_file), "--id", "acceptance_pure")
        eventually("pure planner completed without a grant", lambda: task(pure["id"])["state"] == "completed")
        check("pure planner retained summary", task(pure["id"])["last_output"] == "PURE_PROGRAM_COMPLETE")
        command("tasks", "verify", pure["id"])
        program_file = root / "controller.algal.json"
        program_file.write_bytes((Path(__file__).resolve().parent.parent / "examples/project-controller.algal.json").read_bytes())
        check("agent cells require explicit managed admission", command("backlog", "program", conversation,
              str(program_file), ok=False)[0] != 0)
        check("managed admission requires project authority", command("backlog", "program", conversation,
              str(program_file), "--managed-calls", "2", ok=False)[0] != 0)
        value("projects", "configure", conversation, "Exercise bounded controller admission", "--tasks", "2", "--hours", "1")
        program_args = ("backlog", "program", conversation, str(program_file), "--managed-calls", "2", "--id", "acceptance_controller")
        program = value(*program_args)
        def program_status():
            return value("backlog", "program-status", program["id"])
        eventually("controller published one linked child", lambda: program_status().get("child") is not None)
        suspended = program_status()
        child_id = suspended["child"]
        check("controller waiting with receipt", suspended["calls"] == 1 and suspended["maxCalls"] == 2
              and suspended["phase"] == "waiting for child" and suspended["receipt"].startswith("sha256:"))
        check("program retry returns exact parent", value(*program_args)["id"] == program["id"])
        check("retry does not duplicate call", program_status()["child"] == child_id and program_status()["calls"] == 1)
        check("child resolves parent inspector", value("backlog", "program-status", child_id)["parent"] == program["id"])
        check("parallel project controller rejected", command("backlog", "program", conversation, str(program_file),
              "--managed-calls", "2", "--id", "conflicting_controller", ok=False)[0] != 0)
        check("program inputs reject steering", command("steer", program["id"], "Change pinned program", ok=False)[0] != 0)
        policies = value("projects")
        policy = next(row for row in policies if row["conversation"] == conversation)
        check("exactly one grant task consumed", policy["admitted_tasks"] == 1)
        # File edits cannot mutate a registered manifest or its suspended call.
        program_file.write_text("{}")
        check("changed source retry rejected", command(*program_args, ok=False)[0] != 0)
        daemon.send_signal(signal.SIGTERM)
        check("owned daemon joins before restart", daemon.wait(timeout=25) == 0)
        daemon = None
        start_daemon()
        recovered = program_status()
        check("restart retains exact child and checkpoint", all(recovered[key] == suspended[key]
              for key in ("parent", "calls", "maxCalls", "child", "receipt")))
        command("tasks", "verify", program["id"])
        command("tasks", "verify", child_id)
        before_program_ui = len(value("backlog", "--conversation", conversation))
        _, program_segments = terminal("program-tui", ["chat", "--resume", conversation], [
            ((f"/program {program['id']}\r").encode(), .5), (b"\x1b", .2),
            (b"/program\r", .5), (b"\x1b", .2),
            ((f"cancel {program['id']}\r").encode(), .5), (b"\x04", .3),
        ], redraw_at=(0, 2), durable_at={4: lambda: task(program["id"])["state"] == "cancelled"})
        inspector = re.sub(r"\s+", "", program_segments[0])
        check("TUI program inspector shows linked call and attention", all(text in inspector
              for text in (program["id"], child_id, "1/2", "/attention", suspended["receipt"])))
        check("TUI program picker rendered", "Recentmanagedprograms" in re.sub(r"\s+", "", program_segments[2]))
        check("program UI creates no ordinary task", len(value("backlog", "--conversation", conversation)) == before_program_ui)
        check("parent cancellation settles linked child", task(child_id)["state"] == "cancelled")
        check("cancelled controller launches no second call", program_status()["calls"] == 1)
        command("tasks", "verify", program["id"])
        command("tasks", "verify", child_id)
        check("program acceptance leaves accounts inactive", value("accounts")["accounts"] == [])
        check("program acceptance creates no provider session", value("sessions") == [])
        check("candidate binary unchanged", hashlib.sha256(binary.read_bytes()).hexdigest() == sha)
    except Exception as error:
        errors.append(f"{type(error).__name__}: {error}")
    finally:
        if schedule is not None and daemon is not None and daemon.poll() is None:
            try:
                value("schedules", "pause", schedule["id"], "--revision", str(schedule["revision"]))
            except Exception as error:
                errors.append(f"keepalive pause: {error}")
        if daemon is not None:
            if daemon.poll() is None:
                daemon.send_signal(signal.SIGTERM)
                try:
                    code = daemon.wait(timeout=25)
                    results.append(dict(check="owned daemon graceful shutdown", passed=code == 0, exit=code, pid=daemon.pid))
                except subprocess.TimeoutExpired:
                    results.append(dict(check="owned daemon graceful shutdown", passed=False, pid=daemon.pid, note="Exact owned daemon retained for investigation; timeout is failure."))
            else:
                results.append(dict(check="owned daemon stayed alive until shutdown", passed=False, exit=daemon.returncode, pid=daemon.pid))
        passed = not errors and all(row.get("passed", True) for row in results)
        evidence = dict(schema="xcb.inbox-acceptance.v1", passed=passed, binary=str(binary), sha256=sha,
                        root=str(root), started_at_ms=started, ended_at_ms=time.time_ns() // 1_000_000,
                        results=results, errors=errors, scope="isolated operator acceptance; no live provider delivery")
        (root / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
        print(json.dumps(dict(passed=passed, evidence=str(root / "evidence.json"), checks=sum("check" in row for row in results), errors=errors)))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
