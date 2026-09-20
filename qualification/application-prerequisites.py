#!/usr/bin/env python3
"""Collect and bundle actual native application qualification prerequisites.

Run collect through the host scheduler; it runs fixed Cargo gates and the
credential-free Claude kernel probe. Bundle only rechecks existing captured
bytes and their original times. Neither operation launches an authenticated
provider, performs a model turn, or grants application admission.
"""
import argparse
import ast
import datetime
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time

MAX_AGE_MS = 24 * 60 * 60 * 1000
MAX_LOG = 4 * 1024 * 1024
MAX_JSON = 64 * 1024
SCHEMA = "xcb.application-gate-capture.v1"
CONTEXT_FIELDS = {"version", "runtime_version", "runtime_sha256", "provider", "provider_version", "provider_sha256", "os", "arch", "policy_sha256", "config_sha256", "account", "model"}
GATES = (
    ("fmt", ("fmt", "--all", "--", "--check")),
    ("workspace", ("test", "--workspace", "--locked")),
    ("clippy", ("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")),
    ("release", ("build", "--release", "--locked", "-p", "xcb-cli", "--message-format=json")),
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def is_sha(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def now_ms():
    return time.time_ns() // 1_000_000


def identity(value):
    return (value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_gid,
            value.st_nlink, value.st_size, value.st_mtime_ns, value.st_ctime_ns,
            getattr(value, "st_flags", 0))


def read_file(path, limit, private=False):
    path = Path(path)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC), "rb") as file:
        before = os.fstat(file.fileno())
        require(stat.S_ISREG(before.st_mode) and 0 <= before.st_size <= limit, "file kind or byte bound")
        require(identity(before) == identity(path.lstat()), "file name changed")
        if private:
            require(before.st_uid == os.getuid() and before.st_nlink == 1 and before.st_mode & 0o077 == 0, "capture file is not private")
        data = file.read(limit + 1)
        require(len(data) == before.st_size and identity(before) == identity(os.fstat(file.fileno()))
                and identity(before) == identity(path.lstat()), "file changed while reading")
        return data


def file_hash(path, limit=512 * 1024 * 1024):
    return sha(read_file(path, limit))


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON field")
        result[key] = value
    return result


def decode(data):
    return json.loads(data, object_pairs_hook=no_duplicates,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite JSON")))


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()


def private_directory(path, create=False):
    path = Path(path).absolute()
    if create:
        require(not path.exists() and not path.is_symlink(), "output already exists")
        require(path.parent.resolve(strict=True) == path.parent, "noncanonical output parent")
        path.mkdir(mode=0o700)
    info = path.lstat()
    require(path.resolve(strict=True) == path and stat.S_ISDIR(info.st_mode)
            and info.st_uid == os.getuid() and info.st_mode & 0o077 == 0, "directory is not private/canonical")
    return path


def write_once(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    with os.fdopen(fd, "wb") as file:
        file.write(data)
        file.flush()
        os.fsync(file.fileno())
    directory = os.open(Path(path).parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def artifact(directory, data):
    require(len(data) <= MAX_LOG, "artifact byte bound")
    digest = sha(data)
    path = directory / (digest + ".json")
    if path.exists():
        require(read_file(path, MAX_LOG, True) == data, "existing artifact changed")
    else:
        write_once(path, data)
    return digest


def group_absent(pid):
    try:
        os.killpg(pid, 0)
        return False
    except ProcessLookupError:
        return True
    except PermissionError:
        return False


def run(argv, cwd, timeout, maximum=MAX_LOG):
    """Bounded fixed-command capture; never signal a reaped/reusable PGID.

    A nonzero, incomplete, interrupted or unjoined command cannot produce a
    successful capture. The outer HRA owner retains broader descendant custody;
    this receipt never substitutes for native tests' own process-join proofs.
    """
    started = now_ms()
    mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    try:
        child = subprocess.Popen(argv, cwd=cwd, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 start_new_session=True,
                                 preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK, mask))
    except BaseException:
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        raise
    output = bytearray()
    reaped = False
    complete = False
    try:
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        deadline = time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while selector.get_map():
                require(time.monotonic() < deadline, "command deadline")
                for key, _ in selector.select(min(0.2, max(0.01, deadline - time.monotonic()))):
                    data = os.read(key.fileobj.fileno(), 8192)
                    if not data:
                        selector.unregister(key.fileobj)
                    else:
                        require(len(output) + len(data) <= maximum, "command output bound")
                        output.extend(data)
        wait_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        try:
            # Keep catchable handlers outside waitpid -> returncode -> ownership
            # bookkeeping. Bound this critical section even after pipe EOF.
            child.wait(timeout=min(5, max(0.01, deadline - time.monotonic())))
            reaped = True
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, wait_mask)
        require(group_absent(child.pid), "command group remains; output retained")
        complete = True
    finally:
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        if not reaped and child.returncode is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        child.stdout.close()
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
    require(complete, "capture incomplete")
    return {"argv": list(argv), "exit_code": child.returncode,
            "started_at_ms": started, "finished_at_ms": now_ms(), "output_sha256": sha(output)}, bytes(output)


def inspect(args):
    executable = args.xcb.resolve(strict=True)
    before = file_hash(executable)
    argv = [str(executable), "--json", "--state", str(args.state.resolve(strict=True)),
            "qualify-application", "--inspect", "--account", args.account, "--model", args.model]
    result, output = run(argv, args.source, 20, MAX_JSON)
    require(result["exit_code"] == 0, "xcb qualification inspection failed; doctor the exact final executable first")
    value = decode(output)
    require(isinstance(value, dict) and set(value) == CONTEXT_FIELDS and value["version"] == 1, "unexpected inspection schema")
    require(value["provider"] == args.provider and value["account"] == args.account and value["model"] == args.model, "inspection selection mismatch")
    for key in ("runtime_sha256", "provider_sha256", "policy_sha256", "config_sha256"):
        require(is_sha(value[key]), "inspection digest shape")
    require(value["runtime_sha256"] == before == file_hash(executable), "xcb executable changed")
    return value


def test_runtime_fingerprint(name, source):
    selected = shutil.which(name)
    require(selected is not None and Path(selected).is_absolute(),
            "native interoperability runtime unavailable on absolute PATH: " + name)
    selected = Path(selected)
    executable = selected.resolve(strict=True)
    before = file_hash(executable)
    record, output = run([str(executable), "--version"], source, 5, 4096)
    require(record["exit_code"] == 0 and output.strip(),
            "native interoperability runtime identification failed: " + name)
    current = shutil.which(name)
    require(current == str(selected) and selected.resolve(strict=True) == executable
            and file_hash(executable) == before, "native interoperability runtime changed: " + name)
    return (name, str(selected), str(executable), before, output.decode("utf-8"))


def source_fingerprint(source, cargo):
    # Rust's actual workspace gates also execute TypeScript lock owners through
    # Node and Bun. Bind their source/import/config inputs as well as crates.
    inputs = ("Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml", ".cargo", "crates",
              "src", "package.json", "bun.lock", "bun.lockb", "package-lock.json", "npm-shrinkwrap.json",
              "yarn.lock", "pnpm-lock.yaml", ":(glob)tsconfig*.json", "bunfig.toml",
              "qualification/application-prerequisites.py")
    result, output = run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--",
                          *inputs], source, 10, MAX_LOG)
    require(result["exit_code"] == 0, "cannot enumerate native source inputs")
    names = set(output.rstrip(b"\0").split(b"\0"))
    # Local configuration still affects execution even if ignored by Git.
    for optional in ("rust-toolchain", "rust-toolchain.toml", ".cargo/config", ".cargo/config.toml",
                     "package.json", "bun.lock", "bun.lockb", "package-lock.json", "npm-shrinkwrap.json",
                     "yarn.lock", "pnpm-lock.yaml", "bunfig.toml"):
        if (source / optional).exists() or (source / optional).is_symlink():
            names.add(optional.encode())
    for config in source.glob("tsconfig*.json"):
        names.add(config.name.encode())
    names = sorted(names)
    require(b"Cargo.toml" in names and b"Cargo.lock" in names and 1 <= len(names) <= 10_000, "native source enumeration")
    hashes = []
    for name in names:
        text = name.decode("utf-8")
        path = Path(text)
        require(not path.is_absolute() and ".." not in path.parts, "source path escape")
        hashes.append((text, file_hash(source / path, 16 * 1024 * 1024)))
    toolchain = []
    rustc = Path(os.environ.get("RUSTC") or shutil.which("rustc") or "/opt/homebrew/bin/rustc").absolute()
    for tool in (cargo.absolute(), rustc):
        result, output = run([str(tool), "--version", "--verbose"], source, 15, MAX_JSON)
        require(result["exit_code"] == 0, "toolchain identification failed")
        toolchain.append((str(tool), file_hash(tool.resolve(strict=True)), output.decode("utf-8")))
    runtimes = [test_runtime_fingerprint(name, source) for name in ("node", "bun")]
    # These values are hashed only, never included in public output/artifacts.
    environment = sorted((key, value) for key, value in os.environ.items()
                         if key.startswith(("CARGO_", "RUST")))
    return sha(encoded({"files": hashes, "toolchain": toolchain, "runtimes": runtimes, "environment": environment}))


def required_cases(source, constant):
    text = read_file(source / "crates/xcb-runtime/src/application_qualification.rs", 256 * 1024).decode()
    match = re.search(r"const " + constant + r":\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.S)
    require(match is not None, "runtime prerequisite case schema changed")
    cases = re.findall(r'"([A-Za-z0-9_:]+)"', match.group(1))
    require(cases and len(cases) == len(set(cases)), "runtime prerequisite cases")
    return cases


def check_tests(output, cases):
    text = output.decode("utf-8")
    lines = text.splitlines()
    require(not any(line.startswith("test result: FAILED.") for line in lines), "failed test output")
    require(any(line.startswith("test result: ok.") and "; 0 failed;" in line for line in lines), "missing zero-failure test summary")
    require(all(lines.count("test " + case + " ... ok") == 1 for case in cases), "missing, duplicate, or skipped mandatory application test")


def observed_time(value):
    require(isinstance(value, str), "boundary collection timestamp")
    date = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(date.tzinfo is not None, "boundary time must include timezone")
    return int(date.timestamp() * 1000)


def probe_groups(source):
    tree = ast.parse(read_file(source / "qualification/claude-kernel.py", 256 * 1024))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "GROUPS" for target in node.targets):
            groups = ast.literal_eval(node.value)
            require(isinstance(groups, tuple) and groups and all(isinstance(x, str) for x in groups), "Claude probe group schema")
            return groups
    raise ValueError("Claude probe group schema changed")


def validate_boundary(data, context, source):
    value = decode(data)
    require(isinstance(value, dict), "native boundary object required")
    provider = context["provider"]
    if provider == "claude":
        require(value.get("schema_version") == 1 and value.get("kind") == "kernel-syscall-policy-probe-not-provider-runtime"
                and value.get("credential_free") is True and value.get("live_provider_qualification") is False, "Claude kernel evidence scope")
        reference = value["runtime_reference"]
        require(reference["version"] == context["provider_version"] and reference["executable_sha256"] == context["provider_sha256"]
                and reference["unchanged"] is True, "Claude boundary runtime changed")
        require(value["sandbox_source_sha256"] == context["policy_sha256"] == file_hash(source / "crates/xcb-runtime/src/sandbox.rs"), "Claude policy drift")
        require(value["harness_sha256"] == file_hash(source / "qualification/claude-kernel.py") and value["source_unchanged"] is True, "Claude harness drift")
        require(value["platform"] == "darwin" and context["os"] == "macos"
                and {"arm64": "aarch64", "x86_64": "x86_64"}.get(value["architecture"]) == context["arch"], "Claude platform mismatch")
        result = value["result"]
        require(result["exit_code"] == 0 and result["process_group_joined"] is True and result["stdio_joined"] is True
                and "failure" not in result, "Claude boundary did not finish")
        groups = probe_groups(source)
        require(set(groups) == {"consumer", "peer-account", "ambient-config", "shared-global-temp"}
                and len(groups) == 4 and value.get("shared_temp_ancestors_unchanged") is True, "missing shared Claude temp denial/ancestor proof")
        wanted = {"scratch_write_read": True, "scratch_rename": True, "scratch_unlink": True,
                  "fork_allowed": True, "shell_exec_denied": False}
        for group in groups:
            for action in ("read", "write", "symlink_read", "symlink_write", "hardlink_create", "rename", "overwrite_rename", "unlink", "chmod", "new_file", "directory_list"):
                wanted[group + ":" + action] = False
        wanted["shared-global-temp:tmp_alias_read"] = False
        wanted["shared-global-temp:tmp_alias_write"] = False
        rows = result["checks"]
        require(isinstance(rows, list) and len(rows) == len(wanted) and {row["label"] for row in rows} == set(wanted), "incomplete Claude boundary cases")
        for row in rows:
            require(row.get("matched") is True, "failed Claude boundary case")
            if row["label"] not in ("fork_allowed", "shell_exec_denied"):
                expected = wanted[row["label"]]
                require(row.get("expected_allowed") is expected and row.get("allowed") is expected, "Claude observation mismatch")
                if not expected:
                    require(type(row.get("errno")) is int and row.get("errno") in (errno.EACCES, errno.EPERM), "Claude denial is not permission enforcement")
        protected = value["protected"]
        require(set(protected) == {g + ":" + kind for g in groups for kind in ("file", "directory")}, "missing protected canary")
        for label, row in protected.items():
            before, after = row["identity_before"], row["identity_after"]
            require(isinstance(before, list) and len(before) == 10 and all(type(item) is int for item in before) and before == after and row["identity_unchanged"] is True, "protected metadata changed")
            if label.endswith(":file"):
                require(is_sha(row["sha256_before"]) and row["sha256_before"] == row["sha256_after"] and row["bytes_unchanged"] is True, "protected bytes changed")
        return observed_time(value["observed_at"])
    if provider == "codex":
        require(value.get("schema") == "xcb.codex-native-boundary.v1" and value["version"] == context["provider_version"]
                and value["binarySha256"] == context["provider_sha256"], "Codex boundary identity")
        require(value["platform"] == {("macos", "aarch64"): "macos-arm64"}.get((context["os"], context["arch"])), "Codex platform mismatch")
        sandbox = read_file(source / "crates/xcb-runtime/src/sandbox.rs", 256 * 1024).decode()
        start = sandbox.index("pub fn codex_seatbelt(")
        function = sandbox[start:sandbox.index("\n}\n", start) + 3]
        require(value["sandboxFunctionSha256"] == sha(function.encode()), "Codex policy changed")
        require(value["providerProbeSha256"] == file_hash(source / "qualification/codex-native.py")
                and value["kernelProbeSha256"] == file_hash(source / "qualification/codex-kernel.py"), "Codex probe changed")
        for key, count in (("providerChecks", 32), ("kernelChecks", 19)):
            rows = value[key]
            require(len(rows) == count and len({row["label"] for row in rows}) == count, "incomplete Codex cases")
            require(all(row.get("matched") is True and (row.get("expectedAllowed") is None or row.get("allowed") is row["expectedAllowed"]) for row in rows), "failed Codex case")
        require(value["rootExitCode"] == 0 and all(value[key] is True for key in ("stdioJoined", "processGroupAbsent", "protectedFilesUnchanged", "protectedMetadataUnchanged")), "Codex boundary custody/effect failure")
        # This sanitized schema records only a UTC date. Keep its conservative
        # midnight time; never upgrade it to the bundling time.
        return observed_time(value["observedDate"] + "T00:00:00+00:00")
    if provider == "devin":
        require(value.get("schema") == 2 and value["runtime_version"] == context["provider_version"]
                and value["provider_sha256"] == context["provider_sha256"] and value["helper_sha256"] == context["runtime_sha256"], "Devin exact provider/helper mismatch")
        require(value["credential_free"] is True and value["live_provider_qualification"] is False
                and value["harness_sha256"] == file_hash(source / "qualification/devin-native.ts"), "Devin fixture scope/source")
        require(value["host"]["platform"] == "darwin" and context["os"] == "macos"
                and {"arm64": "aarch64", "x86_64": "x86_64"}.get(value["host"]["arch"]) == context["arch"], "Devin platform mismatch")
        rows = value["scenarios"]
        require(len(rows) == 5 and {r["scenario"] for r in rows} == {"broker", "exec", "write", "config_write", "webfetch"}, "incomplete Devin scenarios")
        for row in rows:
            require(row["status"] == "passed" and row["process_joined"] is True and row["bridge_joined"] is True
                    and row["provider_sha256"] == context["provider_sha256"] and row["helper_sha256"] == context["runtime_sha256"], "Devin scenario/custody mismatch")
            checks = row["checks"]
            require(checks["capture_error"] is None and all(checks[key] is True for key in ("no_canary_leak", "no_native_webfetch", "exact_inventory", "expected_steps", "no_overflow")), "Devin boundary observations failed")
            require(row["stopped"]["code"] == 0 and row["stopped"]["signal"] is None, "Devin fixture did not exit successfully")
        return observed_time(value["observed_at"])
    raise ValueError("unsupported native boundary schema")


def release_executable(output, source):
    """Select Cargo's actual executable, including configured alternate targets."""
    binaries = []
    finished = []
    for line in output.decode("utf-8").splitlines():
        if not line.startswith("{"):
            continue
        value = decode(line)
        require(isinstance(value, dict), "Cargo JSON message shape")
        if value.get("reason") == "build-finished":
            finished.append(value.get("success"))
        if value.get("reason") != "compiler-artifact":
            continue
        target = value.get("target", {})
        if target.get("name") == "xcb" and target.get("kind") == ["bin"]:
            require(target.get("src_path") == str(source / "crates/xcb-cli/src/main.rs")
                    and value.get("profile", {}).get("test") is False, "unexpected XCB build target")
            executable = value.get("executable")
            require(isinstance(executable, str) and 0 < len(executable) <= 4096
                    and Path(executable).is_absolute(), "missing Cargo executable artifact")
            binaries.append(Path(executable).resolve(strict=True))
    require(len(binaries) == 1 and finished == [True], "release build did not emit one successful XCB artifact")
    return binaries[0]


def collect(args):
    target = private_directory(args.output, True)
    artifacts = private_directory(target / "artifacts", True)
    context = inspect(args)
    source_digest = source_fingerprint(args.source, args.cargo)
    records = []
    outputs = {}
    cargo = args.cargo.absolute()
    require(cargo.exists(), "Cargo executable missing")
    for name, command in GATES:
        record, output = run([str(cargo), *command], args.source, 900)
        record["name"] = name
        record["output_sha256"] = artifact(artifacts, output)
        records.append(record)
        outputs[name] = output
        write_once(target / (name + ".execution.json"), encoded(record))
        require(record["exit_code"] == 0, "native gate failed: " + name)
    release = release_executable(outputs["release"], args.source)
    require(file_hash(release) == context["runtime_sha256"] == file_hash(args.xcb.resolve(strict=True)),
            "release built from tested source differs from --xcb; preserve capture and repeat with the final bytes")
    check_tests(outputs["workspace"], required_cases(args.source, "UNIT_CASES"))
    check_tests(outputs["workspace"], required_cases(args.source, "CONTRACT_CASES"))
    if args.provider == "claude":
        require(args.provider_executable is not None, "Claude collection requires --provider-executable")
        provider = args.provider_executable.resolve(strict=True)
        require(file_hash(provider) == context["provider_sha256"], "Claude provider pin mismatch")
        boundary_path = target / "native-boundary.json"
        record, output = run([sys.executable, str(args.source / "qualification/claude-kernel.py"), "--claude", str(provider), "--output", str(boundary_path)], args.source, 90)
        write_once(target / "boundary.execution.json", encoded({**record, "output_sha256": artifact(artifacts, output)}))
        require(record["exit_code"] == 0, "native boundary probe failed")
        # Probe creates a task-owned synthetic receipt using the caller's umask.
        # It is inside our private directory; require owned physical singlelink
        # bytes before tightening only this freshly generated copy to0600.
        info = boundary_path.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1, "generated boundary custody")
        boundary_path.chmod(0o600)
    else:
        require(args.provider_boundary is not None, "provider requires --provider-boundary")
        boundary_path = args.provider_boundary.resolve(strict=True)
    boundary = read_file(boundary_path, MAX_LOG)
    observed = validate_boundary(boundary, context, args.source)
    require(0 < observed <= now_ms() and now_ms() - observed < MAX_AGE_MS, "native boundary expired")
    boundary_hash = artifact(artifacts, boundary)
    require(inspect(args) == context and source_fingerprint(args.source, args.cargo) == source_digest, "source/runtime changed during collection")
    capture = {"schema": SCHEMA, "context": context, "source_sha256": source_digest,
               "collector_sha256": file_hash(Path(__file__).resolve()),
               "collector_argv": [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]], "gates": records,
               "provider_boundary_sha256": boundary_hash, "provider_observed_at_ms": observed}
    write_once(target / "capture.json", encoded(capture))
    bundle(args, target, target)


def bundle(args, capture_dir, target=None):
    capture_dir = private_directory(capture_dir)
    artifacts_in = private_directory(capture_dir / "artifacts")
    raw_capture = read_file(capture_dir / "capture.json", MAX_JSON, True)
    capture = decode(raw_capture)
    require(set(capture) == {"schema", "context", "source_sha256", "collector_sha256", "collector_argv", "gates", "provider_boundary_sha256", "provider_observed_at_ms"}
            and capture["schema"] == SCHEMA, "unsupported execution capture")
    context = inspect(args)
    require(capture["context"] == context and capture["source_sha256"] == source_fingerprint(args.source, args.cargo)
            and capture["collector_sha256"] == file_hash(Path(__file__).resolve()), "capture source/runtime drift")
    records = capture["gates"]
    require(isinstance(records, list) and [r["name"] for r in records] == [name for name, _ in GATES], "incomplete source gates")
    outputs = {}
    all_data = {}
    for record, (name, expected) in zip(records, GATES):
        require(set(record) == {"name", "argv", "exit_code", "started_at_ms", "finished_at_ms", "output_sha256"}, "gate record schema")
        require(record["argv"][1:] == list(expected) and record["exit_code"] == 0, "gate command/result mismatch")
        require(type(record["started_at_ms"]) is int and type(record["finished_at_ms"]) is int
                and 0 < record["started_at_ms"] <= record["finished_at_ms"] <= now_ms(), "gate times")
        digest = record["output_sha256"]
        require(is_sha(digest), "gate digest")
        raw = read_file(artifacts_in / (digest + ".json"), MAX_LOG, True)
        require(sha(raw) == digest, "captured log changed")
        require(decode(read_file(capture_dir / (name + ".execution.json"), MAX_JSON, True)) == record, "execution receipt mismatch")
        outputs[name] = raw
        all_data[digest] = raw
    require(file_hash(release_executable(outputs["release"], args.source)) == context["runtime_sha256"], "built release artifact changed")
    for constant in ("UNIT_CASES", "CONTRACT_CASES"):
        check_tests(outputs["workspace"], required_cases(args.source, constant))
    digest = capture["provider_boundary_sha256"]
    require(is_sha(digest), "boundary digest")
    native = read_file(artifacts_in / (digest + ".json"), MAX_LOG, True)
    require(sha(native) == digest, "native boundary bytes changed")
    native_time = validate_boundary(native, context, args.source)
    require(native_time == capture["provider_observed_at_ms"], "boundary timestamp changed")
    all_data[digest] = native
    start = min([native_time] + [r["started_at_ms"] for r in records])
    finish = max([native_time] + [r["finished_at_ms"] for r in records])
    require(0 < start <= finish <= now_ms() and now_ms() - start < MAX_AGE_MS, "capture expired; rerun collection")
    combined = b"".join(outputs[name] for name, _ in GATES)
    require(0 < len(combined) <= MAX_LOG, "combined source log too large")
    combined_hash = sha(combined)
    all_data[combined_hash] = combined
    gate = lambda argv, output_hash: {"argv": argv, "exit_code": 0, "output_sha256": output_hash}
    source_argv = capture["collector_argv"]
    require(isinstance(source_argv, list) and 3 <= len(source_argv) <= 64
            and source_argv[2] == "collect" and all(isinstance(arg, str) and 0 < len(arg) <= 4096 and not any(ord(ch) < 32 for ch in arg) for arg in source_argv), "collector command record")
    descriptor = {key: value for key, value in context.items() if key not in ("account", "model")}
    descriptor.update(started_at_ms=start, finished_at_ms=finish,
                      source_gate=gate(source_argv, combined_hash),
                      application_unit=gate(records[1]["argv"], records[1]["output_sha256"]),
                      application_contract=gate(records[1]["argv"], records[1]["output_sha256"]),
                      provider_boundary_sha256=digest)
    if target is None:
        target = private_directory(args.output, True)
        artifacts_out = private_directory(target / "artifacts", True)
        write_once(target / "capture-provenance.json", raw_capture)
    else:
        artifacts_out = artifacts_in
    for hash_value, raw in all_data.items():
        require(artifact(artifacts_out, raw) == hash_value, "artifact publication mismatch")
    write_once(target / "prerequisites.json", encoded(descriptor))
    print(json.dumps({"status": "prerequisites_bundled", "live_qualification": False,
                      "account": args.account, "model": args.model,
                      "runtime_sha256": context["runtime_sha256"], "observed_at_ms": start,
                      "expires_at_ms": start + MAX_AGE_MS, "directory": str(target)}))


def self_test():
    """Hermetic parser/file/bundle tests. Never run Cargo, XCB or a provider."""
    import contextlib
    import io
    import tempfile
    import unittest
    from types import SimpleNamespace
    from unittest.mock import patch

    module = sys.modules[__name__]

    class Tests(unittest.TestCase):
        def setUp(self):
            self.temp = tempfile.TemporaryDirectory(prefix="xcb-prerequisites-test-")
            self.directory = Path(self.temp.name).resolve()

        def tearDown(self):
            self.temp.cleanup()

        def fingerprint_fixture(self):
            source = self.directory / "source"
            source.mkdir()
            tools = self.directory / "tools"
            tools.mkdir()
            for name in ("cargo", "rustc", "node", "bun"):
                executable = tools / name
                executable.write_text("#!/bin/sh\nprintf '%s\\n' 'synthetic-1.0.0'\n")
                executable.chmod(0o700)
            environment = {"HOME": str(self.directory), "PATH": str(tools) + ":/usr/bin:/bin",
                           "RUSTC": str(tools / "rustc"), "CARGO_INCREMENTAL": "0",
                           "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
            with patch.dict(os.environ, environment, clear=True):
                record, _ = run(["git", "init", "-q"], source, 5, 4096)
                self.assertEqual(record["exit_code"], 0)
                for name in ("Cargo.toml", "Cargo.lock", "package.json", "bun.lock", "tsconfig.json", "bunfig.toml",
                             "src/cli/write-coordination.ts", "src/private-file.ts"):
                    target = source / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text("synthetic initial input\n")
                record, _ = run(["git", "add", "--", "."], source, 5, 4096)
                self.assertEqual(record["exit_code"], 0)
            return source, tools, environment

        def test_fingerprint_tracks_actual_git_typescript_inputs(self):
            source, tools, environment = self.fingerprint_fixture()
            with patch.dict(os.environ, environment, clear=True):
                before = source_fingerprint(source, tools / "cargo")
                self.assertEqual(source_fingerprint(source, tools / "cargo"), before)
                for name in ("src/cli/write-coordination.ts", "src/private-file.ts"):
                    path = source / name
                    original = path.read_bytes()
                    path.write_bytes(original + b"changed imported code\n")
                    self.assertNotEqual(source_fingerprint(source, tools / "cargo"), before)
                    path.write_bytes(original)
                (source / "src/new-import.ts").write_text("new untracked source\n")
                self.assertNotEqual(source_fingerprint(source, tools / "cargo"), before)

        def test_fingerprint_tracks_package_lock_and_ignored_local_configs(self):
            source, tools, environment = self.fingerprint_fixture()
            with patch.dict(os.environ, environment, clear=True):
                before = source_fingerprint(source, tools / "cargo")
                for name in ("package.json", "bun.lock", "tsconfig.json", "bunfig.toml"):
                    path = source / name
                    original = path.read_bytes()
                    path.write_bytes(original + b"changed input\n")
                    self.assertNotEqual(source_fingerprint(source, tools / "cargo"), before)
                    path.write_bytes(original)
                (source / ".gitignore").write_text("tsconfig.local.json\n")
                (source / "tsconfig.local.json").write_text("ignored runtime configuration\n")
                self.assertNotEqual(source_fingerprint(source, tools / "cargo"), before)

        def test_fingerprint_tracks_runtime_bytes_version_and_path_selection(self):
            source, tools, environment = self.fingerprint_fixture()
            with patch.dict(os.environ, environment, clear=True):
                before = source_fingerprint(source, tools / "cargo")
                for name in ("node", "bun"):
                    path = tools / name
                    original = path.read_bytes()
                    # Same --version output must not hide executable changes.
                    path.write_bytes(original + b"# changed executable bytes\n")
                    self.assertNotEqual(source_fingerprint(source, tools / "cargo"), before)
                    path.write_bytes(original)
                alternate = self.directory / "alternate"
                alternate.mkdir()
                (alternate / "node").write_bytes((tools / "node").read_bytes())
                (alternate / "node").chmod(0o700)
                os.environ["PATH"] = str(alternate) + ":" + environment["PATH"]
                self.assertNotEqual(source_fingerprint(source, tools / "cargo"), before)
                os.environ["PATH"] = environment["PATH"]
                # Version output is also evidence, independent of executable bytes.
                (tools / "node").write_text("#!/bin/sh\nprintf '%s\\n' \"${SYNTHETIC_VERSION:-one}\"\n")
                first = source_fingerprint(source, tools / "cargo")
                os.environ["SYNTHETIC_VERSION"] = "two"
                self.assertNotEqual(source_fingerprint(source, tools / "cargo"), first)

        def test_fingerprint_refuses_missing_required_interop_runtime(self):
            source, tools, environment = self.fingerprint_fixture()
            with patch.dict(os.environ, {**environment, "PATH": str(tools)}, clear=True):
                (tools / "bun").unlink()
                with self.assertRaisesRegex(ValueError, "runtime unavailable"):
                    test_runtime_fingerprint("bun", source)

        def test_json_is_closed_against_duplicates_and_nonfinite_values(self):
            for raw in (b'{"version":1,"version":1}', b'{"value":NaN}'):
                with self.assertRaises(ValueError):
                    decode(raw)

        def test_private_file_refuses_aliases_permissions_and_fifo_without_blocking(self):
            path = self.directory / "file"
            write_once(path, b"synthetic")
            self.assertEqual(read_file(path, 20, True), b"synthetic")
            alias = self.directory / "alias"
            alias.symlink_to(path)
            with self.assertRaises(OSError):
                read_file(alias, 20, True)
            alias.unlink()
            os.link(path, alias)
            with self.assertRaises(ValueError):
                read_file(path, 20, True)
            alias.unlink()
            path.chmod(0o644)
            with self.assertRaises(ValueError):
                read_file(path, 20, True)
            fifo = self.directory / "fifo"
            os.mkfifo(fifo, 0o600)
            with self.assertRaises(ValueError):
                read_file(fifo, 20, True)

        def test_all_cases_must_be_successful_and_unambiguous(self):
            good = b"test synthetic ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"
            check_tests(good, ["synthetic"])
            for bad in (good.replace(b"... ok", b"... ignored"), good + b"test synthetic ... ok\n", good + b"test result: FAILED.\n", b"test result: ok. 0 passed; 0 failed;\n"):
                with self.assertRaises(ValueError):
                    check_tests(bad, ["synthetic"])

        def test_child_does_not_inherit_blocked_cancellation_signals(self):
            program = "import json,signal; print(json.dumps(sorted(int(x) for x in signal.pthread_sigmask(signal.SIG_BLOCK, set()))))"
            record, output = run([sys.executable, "-I", "-c", program], self.directory, 5, 4096)
            self.assertEqual(record["exit_code"], 0)
            blocked = decode(output)
            self.assertNotIn(int(signal.SIGTERM), blocked)
            self.assertNotIn(int(signal.SIGINT), blocked)

        def fixture(self):
            capture = private_directory(self.directory / "capture", True)
            artifacts = private_directory(capture / "artifacts", True)
            context = {"version": 1, "runtime_version": "synthetic", "runtime_sha256": "1" * 64,
                       "provider": "claude", "provider_version": "synthetic", "provider_sha256": "2" * 64,
                       "os": "macos", "arch": "aarch64", "policy_sha256": "3" * 64,
                       "config_sha256": "4" * 64, "account": "a_synthetic", "model": "claude/synthetic"}
            stamp = now_ms() - 1000
            records = []
            executable = self.directory / "alternate-target/release/xcb"
            executable.parent.mkdir(parents=True)
            write_once(executable, b"synthetic-release-binary")
            for name, command in GATES:
                output = b"test synthetic ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n" if name == "workspace" else b""
                if name == "release":
                    output = (json.dumps({"reason": "compiler-artifact", "target": {"name": "xcb", "kind": ["bin"], "src_path": str(self.directory / "crates/xcb-cli/src/main.rs")}, "profile": {"test": False}, "executable": str(executable)}) + "\n" + json.dumps({"reason": "build-finished", "success": True}) + "\n").encode()
                record = {"name": name, "argv": ["/synthetic/cargo", *command], "exit_code": 0,
                          "started_at_ms": stamp, "finished_at_ms": stamp + 1,
                          "output_sha256": artifact(artifacts, output)}
                write_once(capture / (name + ".execution.json"), encoded(record))
                records.append(record)
            native = artifact(artifacts, b'{"synthetic_test_only":true}')
            manifest = {"schema": SCHEMA, "context": context, "source_sha256": "5" * 64,
                        "collector_sha256": file_hash(Path(__file__).resolve()),
                        "collector_argv": [sys.executable, str(Path(__file__).resolve()), "collect"],
                        "gates": records, "provider_boundary_sha256": native, "provider_observed_at_ms": stamp}
            write_once(capture / "capture.json", encoded(manifest))
            args = SimpleNamespace(output=self.directory / "bundle", source=self.directory,
                                   account="a_synthetic", model="claude/synthetic", cargo=Path("/synthetic/cargo"))
            return capture, context, manifest, args

        def invoke(self, args, capture, context, stamp):
            hash_file = file_hash
            def synthetic_hash(path, limit=512 * 1024 * 1024):
                return context["runtime_sha256"] if Path(path).name == "xcb" else hash_file(path, limit)
            with patch.object(module, "file_hash", side_effect=synthetic_hash), patch.object(module, "inspect", return_value=context), patch.object(module, "source_fingerprint", return_value="5" * 64), patch.object(module, "required_cases", return_value=["synthetic"]), patch.object(module, "validate_boundary", return_value=stamp), contextlib.redirect_stdout(io.StringIO()):
                bundle(args, capture)

        def test_release_identity_uses_actual_cargo_artifact_with_alternate_target(self):
            capture, context, manifest, args = self.fixture()
            record = manifest["gates"][-1]
            output = read_file(capture / "artifacts" / (record["output_sha256"] + ".json"), MAX_LOG, True)
            self.assertEqual(release_executable(output, self.directory), self.directory / "alternate-target/release/xcb")
            with self.assertRaises(ValueError):
                release_executable(output + output, self.directory)
            with self.assertRaises(ValueError):
                release_executable(output.replace(b'"success": true', b'"success": false'), self.directory)

        def test_bundle_preserves_original_raw_bytes_and_expiry(self):
            capture, context, manifest, args = self.fixture()
            self.invoke(args, capture, context, manifest["provider_observed_at_ms"])
            descriptor = decode(read_file(args.output / "prerequisites.json", MAX_JSON, True))
            self.assertEqual(descriptor["started_at_ms"], manifest["provider_observed_at_ms"])
            self.assertEqual(descriptor["application_unit"], descriptor["application_contract"])
            self.assertNotIn("passed", descriptor)
            raw = read_file(args.output / "artifacts" / (descriptor["application_unit"]["output_sha256"] + ".json"), MAX_LOG, True)
            self.assertEqual(raw, b"test synthetic ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n")
            self.assertEqual((args.output / "prerequisites.json").stat().st_mode & 0o777, 0o600)
            self.assertEqual(args.output.stat().st_mode & 0o777, 0o700)

        def test_bundle_rejects_changed_log_and_leaves_no_descriptor(self):
            capture, context, manifest, args = self.fixture()
            (capture / "artifacts" / (manifest["gates"][1]["output_sha256"] + ".json")).write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                self.invoke(args, capture, context, manifest["provider_observed_at_ms"])
            self.assertFalse(args.output.exists())

        def test_bundle_cannot_retimestamp_expired_capture(self):
            capture, context, manifest, args = self.fixture()
            stamp = now_ms() - MAX_AGE_MS - 1000
            manifest["provider_observed_at_ms"] = stamp
            (capture / "capture.json").write_bytes(encoded(manifest))
            with self.assertRaises(ValueError):
                self.invoke(args, capture, context, stamp)
            self.assertFalse(args.output.exists())

        def test_nonzero_actual_execution_record_cannot_be_overridden(self):
            capture, context, manifest, args = self.fixture()
            manifest["gates"][0]["exit_code"] = 1
            (capture / "capture.json").write_bytes(encoded(manifest))
            with self.assertRaises(ValueError):
                self.invoke(args, capture, context, manifest["provider_observed_at_ms"])
            self.assertFalse(args.output.exists())

        def test_context_drift_and_unknown_manifest_fields_fail(self):
            capture, context, manifest, args = self.fixture()
            changed = dict(context, runtime_sha256="f" * 64)
            with self.assertRaises(ValueError):
                self.invoke(args, capture, changed, manifest["provider_observed_at_ms"])
            manifest["passed"] = True
            (capture / "capture.json").write_bytes(encoded(manifest))
            with self.assertRaises(ValueError):
                self.invoke(args, capture, context, manifest["provider_observed_at_ms"])

    result = unittest.TextTestRunner(verbosity=1).run(unittest.defaultTestLoader.loadTestsFromTestCase(Tests))
    return 0 if result.wasSuccessful() else 1


def main():
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("collect", "bundle"))
    parser.add_argument("--xcb", required=True, type=Path)
    parser.add_argument("--state", required=True, type=Path)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--provider", required=True, choices=("claude", "codex", "devin"))
    parser.add_argument("--account", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cargo", type=Path, default=Path(shutil.which("cargo") or "/opt/homebrew/bin/cargo"))
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--provider-executable", type=Path)
    parser.add_argument("--provider-boundary", type=Path)
    args = parser.parse_args()
    args.source = args.source.resolve(strict=True)
    try:
        if args.mode == "collect":
            require(args.capture is None, "collect does not import old gate captures")
            collect(args)
        else:
            require(args.capture is not None, "bundle requires --capture")
            bundle(args, args.capture)
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError, UnicodeError) as error:
        print("application prerequisites failed: " + str(error), file=sys.stderr)
        return 1


def interrupted(_number, _frame):
    raise InterruptedError("qualification collection interrupted")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    sys.exit(main())
