"""Bounded synthetic PTY driver; never opens a browser or a real provider."""
import errno
import json
import os
import pty
import select
import signal
import sys
import time
import tty


pid, master = pty.fork()
if pid == 0:
    # Reproduce an inherited UI's raw mode, including CR not becoming LF.
    tty.setraw(0)
    os.execv(sys.argv[1], [sys.argv[1], sys.argv[2]])

output = bytearray()
sent = 0
status = None
deadline = time.monotonic() + 12
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.05)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            output.extend(chunk)
            if len(output) > 131072:
                raise RuntimeError("synthetic PTY output exceeded limit")
            count = output.replace(b"\r", b"").count(b"NEED_CODE\n")
            # A provider receives the same CR produced by a real Enter key.
            while sent < count:
                os.write(master, b"\x03" if sys.argv[3] == "cancel" else b"synthetic-code\r")
                sent += 1
        exited, value = os.waitpid(pid, os.WNOHANG)
        if exited:
            status = value
            # Drain final output before publishing the synthetic receipt.
            while select.select([master], [], [], 0)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                output.extend(chunk)
            break
    while status is None and time.monotonic() < deadline:
        exited, value = os.waitpid(pid, os.WNOHANG)
        if exited:
            status = value
        else:
            time.sleep(0.01)
    if status is None:
        raise RuntimeError("synthetic PTY child did not finish")
    print(json.dumps({"exit": os.waitstatus_to_exitcode(status), "sent": sent,
                      "output": output.decode("utf-8", "replace")}))
finally:
    if status is None:
        # Only the synthetic session created above is signalled on failure.
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
    os.close(master)
