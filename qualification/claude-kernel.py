#!/usr/bin/env python3
"""Synthetic syscall probe for the current Rust Claude Seatbelt policy."""
import argparse
import codecs
import ctypes
import datetime
import encodings.idna
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

LIMIT = 65536
DEADLINE = 20
DENIALS = (errno.EACCES, errno.EPERM)
GROUPS = ("consumer", "peer-account", "ambient-config", "shared-global-temp")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def stat_identity(value):
    return [value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_gid,
            value.st_nlink, value.st_mtime_ns, value.st_ctime_ns, value.st_size,
            value.st_flags]


def identity(path):
    return stat_identity(path.lstat())

def stable_setup(paths):
    previous = {label: identity(path) for label, path in paths.items()}
    deadline = time.monotonic() + 5
    quiet_since = time.monotonic()
    while time.monotonic() < deadline:
        time.sleep(0.05)
        current = {label: identity(path) for label, path in paths.items()}
        if current != previous:
            previous = current
            quiet_since = time.monotonic()
        elif time.monotonic() - quiet_since >= 0.25:
            return current
    raise RuntimeError("trusted fixture metadata did not stabilize")


def stable_file(path, maximum):
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK), "rb") as source:
        descriptor = os.fstat(source.fileno())
        before = stat_identity(descriptor)
        if before != identity(path):
            raise ValueError("reference path identity")
        if not stat.S_ISREG(descriptor.st_mode) or descriptor.st_size > maximum:
            raise ValueError("reference file bound or type")
        data = source.read(descriptor.st_size + 1)
        if len(data) != descriptor.st_size or before != stat_identity(os.fstat(source.fileno())) or before != identity(path):
            raise ValueError("reference file changed")
        if (descriptor.st_dev, descriptor.st_ino) != tuple(before[:2]):
            raise ValueError("reference identity")
    return data, before


def extract(source):
    start = source.index("pub fn seatbelt(")
    function = source[start:source.index("\n}\n", start) + 3]
    matches = re.findall(r'r#"(.*?)"#', function, re.S)
    if len(matches) != 1 or set(re.findall(r"\{([^{}]+)\}", matches[0])) != {"exe", "work"}:
        raise ValueError("Claude production template changed")
    return function, matches[0]


def required_labels():
    labels = {"scratch_write_read", "scratch_rename", "scratch_unlink", "fork_allowed", "shell_exec_denied",
              "shared-global-temp:tmp_alias_read", "shared-global-temp:tmp_alias_write"}
    for group in GROUPS:
        labels.update(group + ":" + action for action in (
            "read", "write", "symlink_read", "symlink_write", "hardlink_create",
            "rename", "overwrite_rename", "unlink", "chmod", "new_file", "directory_list"))
    return labels


def directory_authority(value):
    # Shared ancestors may have unrelated legitimate children; compare ownership,
    # inode, permissions and flags, without their mutable entry timestamps/counts.
    return [value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_gid, value.st_flags]


def foreign_directory(directory, group):
    if group == "shared-global-temp":
        return Path("/private/tmp") / ("claude-" + str(os.getuid())) / directory.name
    return directory / group


class SharedTemp:
    """Own only one exclusive synthetic child, retaining descriptors to ancestors."""
    def __init__(self, name):
        self.records = []
        self.child_fd = None
        self.base_fd = None
        self.name = name
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
        try:
            path = Path("/")
            parent_fd = None
            for component in ("/", "private", "tmp", "claude-" + str(os.getuid())):
                if component != "/":
                    path = path / component
                if component.startswith("claude-"):
                    try:
                        os.mkdir(component, 0o700, dir_fd=parent_fd)
                    except FileExistsError:
                        pass
                fd = os.open(component, flags, dir_fd=parent_fd)
                value = os.fstat(fd)
                self.records.append((path, fd, directory_authority(value)))
                expected_uid = os.getuid() if component.startswith("claude-") else 0
                if (not stat.S_ISDIR(value.st_mode) or value.st_uid != expected_uid
                        or (component != "tmp" and value.st_mode & 0o022)
                        or (component == "tmp" and not value.st_mode & stat.S_ISVTX)
                        or directory_authority(path.lstat()) != directory_authority(value)):
                    raise ValueError("shared temporary ancestor authority")
                parent_fd = fd
            self.base_fd = parent_fd
            if not self.unchanged():
                raise ValueError("shared temporary ancestor changed")
            os.mkdir(name, 0o700, dir_fd=self.base_fd)
            self.child_fd = os.open(name, flags, dir_fd=self.base_fd)
            self.path = path / name
            self.child_authority = directory_authority(os.fstat(self.child_fd))
            if (not self.unchanged()
                    or directory_authority(self.path.lstat()) != self.child_authority):
                raise ValueError("shared temporary child changed")
        except BaseException:
            self.close()
            raise

    def unchanged(self):
        try:
            return all(directory_authority(os.fstat(fd)) == before
                       and directory_authority(path.lstat()) == before
                       for path, fd, before in self.records)
        except OSError:
            return False

    def cleanup(self):
        # No traversal or enumeration of the pre-existing global directory.
        # This is called only after joined, successful checks and intact canaries.
        if (not self.unchanged()
                or directory_authority(os.fstat(self.child_fd)) != self.child_authority
                or directory_authority(os.stat(self.name, dir_fd=self.base_fd,
                                                follow_symlinks=False)) != self.child_authority):
            raise RuntimeError("shared temporary child authority changed; retained")
        os.unlink("canary", dir_fd=self.child_fd)
        os.rmdir(self.name, dir_fd=self.base_fd)

    def close(self):
        if self.child_fd is not None:
            os.close(self.child_fd)
            self.child_fd = None
        for _, fd, _ in reversed(self.records):
            os.close(fd)
        self.records = []


def child(directory):
    # All Python imports/codecs are loaded before applying the provider policy.
    for encoding in ("idna", "ascii", "utf-8", "latin-1"):
        codecs.lookup(encoding)
    directory = Path(directory)
    scratch = directory / "scratch"
    policy = (directory / "production.sb").read_bytes()
    if len(policy) > LIMIT:
        raise ValueError("policy bound")
    library = ctypes.CDLL("/usr/lib/libsandbox.1.dylib", use_errno=True)
    library.sandbox_init.argtypes = [ctypes.c_char_p, ctypes.c_uint64,
                                    ctypes.POINTER(ctypes.c_char_p)]
    library.sandbox_init.restype = ctypes.c_int
    error = ctypes.c_char_p()
    if library.sandbox_init(policy, 0, ctypes.byref(error)) != 0:
        print(json.dumps({"failure": "sandbox initialization"}), flush=True)
        return 1
    checks = []

    def check(label, allowed, operation):
        row = {"label": label, "expected_allowed": allowed}
        try:
            operation()
            row["allowed"] = True
        except OSError as exception:
            row["allowed"] = False
            row["errno"] = exception.errno
        row["matched"] = row["allowed"] == allowed and (allowed or row.get("errno") in DENIALS)
        checks.append(row)

    def scratch_read_write():
        path = scratch / "allowed-created"
        path.write_bytes(b"SYNTHETIC_ALLOWED_SCRATCH\n")
        if path.read_bytes() != b"SYNTHETIC_ALLOWED_SCRATCH\n":
            raise ValueError("scratch positive control")

    check("scratch_write_read", True, scratch_read_write)
    check("scratch_rename", True, lambda: os.rename(scratch / "allowed-created", scratch / "allowed-renamed"))
    check("scratch_unlink", True, lambda: (scratch / "allowed-renamed").unlink())
    for group in GROUPS:
        foreign = foreign_directory(directory, group)
        path = foreign / "canary"
        alias = scratch / (group + "-symlink")
        check(group + ":read", False, lambda path=path: path.read_bytes())
        check(group + ":write", False, lambda path=path: path.write_bytes(b"FORBIDDEN_CHANGE"))
        check(group + ":symlink_read", False, lambda alias=alias: alias.read_bytes())
        check(group + ":symlink_write", False, lambda alias=alias: alias.write_bytes(b"FORBIDDEN_CHANGE"))
        check(group + ":hardlink_create", False,
              lambda path=path, group=group: os.link(path, scratch / (group + "-hardlink")))
        check(group + ":rename", False,
              lambda path=path, group=group: os.rename(path, scratch / (group + "-moved")))
        check(group + ":overwrite_rename", False,
              lambda path=path, group=group: os.rename(scratch / (group + "-replacement"), path))
        check(group + ":unlink", False, lambda path=path: path.unlink())
        check(group + ":chmod", False, lambda path=path: path.chmod(0o777))
        check(group + ":new_file", False,
              lambda foreign=foreign: (foreign / "unexpected-new").write_bytes(b"FORBIDDEN_CHANGE"))
        check(group + ":directory_list", False, lambda foreign=foreign: list(foreign.iterdir()))

    global_alias = Path("/tmp") / ("claude-" + str(os.getuid())) / directory.name / "canary"
    check("shared-global-temp:tmp_alias_read", False, lambda: global_alias.read_bytes())
    check("shared-global-temp:tmp_alias_write", False, lambda: global_alias.write_bytes(b"FORBIDDEN_CHANGE"))

    fork_pid = os.fork()
    if fork_pid == 0:
        os._exit(0)
    _, status = os.waitpid(fork_pid, 0)
    checks.append({"label": "fork_allowed", "matched": os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0})
    fork_pid = os.fork()
    if fork_pid == 0:
        try:
            os.execve("/bin/sh", ["/bin/sh", "-c", "exit 0"], {"PATH": "/usr/bin:/bin"})
        except OSError as exception:
            os._exit(77 if exception.errno in DENIALS else 78)
    _, status = os.waitpid(fork_pid, 0)
    checks.append({"label": "shell_exec_denied", "matched": os.WIFEXITED(status) and os.WEXITSTATUS(status) == 77})
    print(json.dumps({"python_version": platform.python_version(), "checks": checks}, separators=(",", ":")), flush=True)
    return 0


def group_absent(pid):
    try:
        os.killpg(pid, 0)
        return False
    except ProcessLookupError:
        return True
    except PermissionError:
        # A zombie-only macOS group may be unsignalable; this is not absence.
        return False


def capture_child(script, directory):
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    try:
        process = subprocess.Popen(
            ["/usr/bin/python3", "-I", str(script), "--child", str(directory)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={"PATH": "/usr/bin:/bin", "HOME": str(directory / "scratch"),
                 "TMPDIR": str(directory / "scratch"), "LANG": "en_US.UTF-8"},
            cwd=directory / "scratch", start_new_session=True)
    except BaseException:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        raise
    captured = {"stdout": bytearray(), "stderr": bytearray()}
    problem = None
    joined = False
    stdio_joined = False
    try:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            selector.register(process.stderr, selectors.EVENT_READ, "stderr")
            deadline = time.monotonic() + DEADLINE
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise TimeoutError()
                for key, _ in selector.select(0.25):
                    data = os.read(key.fileobj.fileno(), 8192)
                    if not data:
                        selector.unregister(key.fileobj)
                    elif len(captured[key.data]) + len(data) > LIMIT:
                        raise OverflowError()
                    else:
                        captured[key.data].extend(data)
            stdio_joined = True
            # Keep the leader unreaped until group cleanup: its PID must not be reused.
    except (Exception, KeyboardInterrupt) as exception:
        problem = "deadline" if isinstance(exception, TimeoutError) else "capture or interruption"
    finally:
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        if not group_absent(process.pid):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                # Still require wait() and a fresh absence check below.
                pass
        try:
            process.wait(timeout=5)
            joined = group_absent(process.pid)
        except subprocess.TimeoutExpired:
            joined = False
        process.stdout.close()
        process.stderr.close()
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
    if not joined:
        raise RuntimeError("child process group exit unproven; scratch retained")
    result = {"exit_code": process.returncode, "process_group_joined": joined, "stdio_joined": stdio_joined,
              "stderr_sha256": sha(captured["stderr"])}
    if problem:
        result["failure"] = problem
    try:
        value = json.loads(captured["stdout"])
        if not isinstance(value, dict) or set(value) != {"python_version", "checks"}:
            raise ValueError()
        if re.fullmatch(r"[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}", value["python_version"]) is None:
            raise ValueError()
        checks = value["checks"]
        if not isinstance(checks, list) or len(checks) != len(required_labels()):
            raise ValueError()
        labels = set()
        for row in checks:
            if not isinstance(row, dict) or not set(row) <= {"label", "matched", "expected_allowed", "allowed", "errno"}:
                raise ValueError()
            if row.get("label") not in required_labels() or row["label"] in labels or type(row.get("matched")) is not bool:
                raise ValueError()
            labels.add(row["label"])
            for key in ("expected_allowed", "allowed"):
                if key in row and type(row[key]) is not bool:
                    raise ValueError()
            if "errno" in row and (type(row["errno"]) is not int or not 0 <= row["errno"] <= 255):
                raise ValueError()
        result.update(value)
    except (ValueError, TypeError, KeyError):
        result["failure"] = "invalid child result"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--claude", type=Path, default=Path.home() / ".bun/bin/claude")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS is required; no unsandboxed fallback")
    script = Path(__file__).resolve()
    source_path = script.parent.parent / "crates/xcb-runtime/src/sandbox.rs"
    source = source_path.read_bytes()
    function, raw = extract(source.decode())
    executable = args.claude.resolve(strict=True)
    executable_bytes, executable_identity = stable_file(executable, 512 * 1024 * 1024)
    package_path = executable.parent / "package.json"
    package_bytes, package_identity = stable_file(package_path, LIMIT)
    package = json.loads(package_bytes)
    version = package.get("version")
    expected_package = {"arm64": "@anthropic-ai/claude-code-darwin-arm64",
                        "x86_64": "@anthropic-ai/claude-code-darwin-x64"}.get(platform.machine())
    if (expected_package is None or package.get("name") != expected_package
            or not isinstance(version, str)
            or re.fullmatch(r"[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}", version) is None):
        raise ValueError("Claude native package metadata")
    script_hash = sha(script.read_bytes())
    directory = Path(tempfile.mkdtemp(prefix="xcb-claude-kernel-")).resolve()
    directory.chmod(0o700)
    scratch = directory / "scratch"
    scratch.mkdir(mode=0o700)
    shared = SharedTemp(directory.name)
    protected = {}
    protected_bytes = {}
    for group in GROUPS:
        foreign = foreign_directory(directory, group)
        if group != "shared-global-temp":
            foreign.mkdir(mode=0o700)
        canary = foreign / "canary"
        contents = ("SYNTHETIC_" + group + "_CANARY\n").encode()
        if group == "shared-global-temp":
            descriptor = os.open("canary", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                                 0o600, dir_fd=shared.child_fd)
            with os.fdopen(descriptor, "wb") as target:
                target.write(contents)
        else:
            canary.write_bytes(contents)
            canary.chmod(0o600)
        protected[group + ":file"] = canary
        protected[group + ":directory"] = foreign
        protected_bytes[group] = contents
        (scratch / (group + "-symlink")).symlink_to(canary)
        (scratch / (group + "-replacement")).write_bytes(b"SYNTHETIC_REPLACEMENT\n")
    control = scratch / "hardlink-control"
    control.write_bytes(b"SYNTHETIC_CONTROL\n")
    os.link(control, scratch / "hardlink-control-alias")
    (scratch / "hardlink-control-alias").unlink()
    control.unlink()
    initial = stable_setup(protected)
    policy = raw.replace("{exe}", json.dumps(str(executable), ensure_ascii=False)).replace("{work}", json.dumps(str(scratch), ensure_ascii=False))
    (directory / "production.sb").write_text(policy)
    completed = False
    passed = False
    try:
        if not shared.unchanged():
            raise RuntimeError("shared temporary ancestors changed; retained")
        result = capture_child(script, directory)
        completed = result["process_group_joined"]
        files = {}
        for label, path in protected.items():
            after = identity(path) if path.exists() else None
            row = {"identity_before": initial[label], "identity_after": after,
                   "identity_unchanged": after == initial[label]}
            if label.endswith(":file"):
                expected = protected_bytes[label.split(":", 1)[0]]
                actual = path.read_bytes() if path.is_file() else b""
                row.update(sha256_before=sha(expected), sha256_after=sha(actual), bytes_unchanged=actual == expected)
            files[label] = row
        reference_unchanged = (identity(executable) == executable_identity
                               and identity(package_path) == package_identity
                               and args.claude.resolve(strict=True) == executable)
        source_unchanged = source_path.read_bytes() == source and sha(script.read_bytes()) == script_hash
        receipt = {"schema_version": 1, "credential_free": True, "live_provider_qualification": False,
                   "kind": "kernel-syscall-policy-probe-not-provider-runtime",
                   "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   "platform": sys.platform, "architecture": platform.machine(),
                   "macos_version": platform.mac_ver()[0],
                   "runtime_reference": {"version": version, "version_source": "installed package metadata; runtime not executed",
                                         "executable_sha256": sha(executable_bytes), "package_sha256": sha(package_bytes),
                                         "unchanged": reference_unchanged},
                   "harness_sha256": script_hash, "sandbox_source_sha256": sha(source),
                   "sandbox_function_sha256": sha(function.encode()), "sandbox_policy_sha256": sha(policy.encode()),
                   "source_unchanged": source_unchanged,
                   "shared_temp_ancestors_unchanged": shared.unchanged(),
                   "result": result, "protected": files}
        receipt["passed"] = (reference_unchanged and source_unchanged
                             and receipt["shared_temp_ancestors_unchanged"] and result["exit_code"] == 0
                             and result["process_group_joined"] and result["stdio_joined"] and "failure" not in result
                             and len(result.get("checks", [])) == len(required_labels())
                             and all(row["matched"] for row in result.get("checks", []))
                             and all(row["identity_unchanged"] and row.get("bytes_unchanged", True) for row in files.values()))
        passed = receipt["passed"]
        output = json.dumps(receipt, indent=2, allow_nan=False) + "\n"
        args.output.write_text(output)
        print(json.dumps({"passed": receipt["passed"], "checks": len(result.get("checks", [])),
                          "failed_checks": [row["label"] for row in result.get("checks", []) if not row["matched"]],
                          "process_group_joined": result["process_group_joined"], "runtime_version": version,
                          "runtime_sha256": sha(executable_bytes), "metadata_unchanged": all(row["identity_unchanged"] for row in files.values()),
                          "bytes_unchanged": all(row.get("bytes_unchanged", True) for row in files.values())}))
        return 0 if receipt["passed"] else 1
    finally:
        try:
            if completed and passed:
                shared.cleanup()
        finally:
            shared.close()
        if completed:
            shutil.rmtree(directory)


def interrupted(_signal, _frame):
    raise KeyboardInterrupt()


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    if len(sys.argv) == 3 and sys.argv[1] == "--child":
        signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM, signal.SIGINT})
        sys.exit(child(sys.argv[2]))
    sys.exit(main())
