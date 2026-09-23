#!/usr/bin/env python3
"""Explicit, pinned macOS Claude qualification renewal. No expiry extension.

Bind while the selected account is qualified. Install is a separate explicit
operation. A run collects NEW prerequisite evidence and uses only the native
fixed challenge. Native XCB continues to own provider/account custody.
"""
import argparse
import contextlib
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid

SCHEMA = "xcb.application-renewal.v1"
MAX_JSON = 64 * 1024
MAX_OUTPUT = 4 * 1024 * 1024
DAY_MS = 24 * 60 * 60 * 1000
RENEW_BEFORE_MS = 12 * 60 * 60 * 1000
MAX_ATTEMPTS = 64
MAX_EVIDENCE_BYTES = 1024 * 1024 * 1024
EXPECTED_GENERATION_FLAG = "--expected-generation"
SYSTEM_PATH = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
CONTEXT_FIELDS = {"version", "runtime_version", "runtime_sha256", "provider", "provider_version", "provider_sha256", "os", "arch", "policy_sha256", "config_sha256", "account", "model"}


class AccountBusy(ValueError):
    pass


def require(ok, message):
    if not ok:
        raise ValueError(message)


def now_ms():
    return time.time_ns() // 1_000_000


def sha(data):
    return hashlib.sha256(data).hexdigest()


def digest(value):
    return isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value) is not None


def pairs(items):
    result = {}
    for key, value in items:
        require(key not in result, "duplicate JSON field")
        result[key] = value
    return result


def decode(data):
    return json.loads(data, object_pairs_hook=pairs,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite JSON")))


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()


def identity(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid,
            info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def physical(path, private=False, directory=False):
    path = Path(path)
    require(path.is_absolute() and path.resolve(strict=True) == path, "path must be physical and absolute")
    info = path.lstat()
    require((stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))
            and info.st_uid in (0, os.getuid()) and info.st_mode & 0o022 == 0,
            "path ownership, kind, or writable mode")
    if private:
        require(info.st_uid == os.getuid() and info.st_mode & 0o077 == 0, "private path required")
    if not directory:
        require(info.st_nlink == 1, "hard-linked file refused")
    return path


def read(path, maximum=MAX_JSON, private=True):
    path = physical(path, private)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as file:
        before = os.fstat(file.fileno())
        require(identity(before) == identity(path.lstat()) and 0 <= before.st_size <= maximum, "file bound or identity")
        data = file.read(maximum + 1)
        require(len(data) == before.st_size and identity(before) == identity(os.fstat(file.fileno()))
                and identity(before) == identity(path.lstat()), "file changed while reading")
        return data


def file_hash(path):
    return sha(read(path, 512 * 1024 * 1024, False))


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_once(path, data):
    physical(Path(path).parent, True, True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    with os.fdopen(fd, "wb") as file:
        file.write(data)
        file.flush()
        os.fsync(file.fileno())
    sync_directory(Path(path).parent)


def record_status(directory, value):
    """Called under owner lock; one bounded, body-free latest status."""
    path = directory / "last-status.json"
    previous = read(path) if path.exists() or path.is_symlink() else None
    data = encoded({"updated_at_ms": now_ms(), **value})
    require(len(data) <= MAX_JSON, "status bound")
    if previous is None:
        write_once(path, data)
        return
    temporary = directory / (".status-" + uuid.uuid4().hex)
    write_once(temporary, data)
    require(read(path) == previous, "status changed outside renewal owner")
    os.replace(temporary, path)
    sync_directory(directory)


def private_directory(path):
    path = Path(path)
    physical(path.parent, directory=True)
    path.mkdir(mode=0o700)
    return physical(path, True, True)


@contextlib.contextmanager
def owner(directory):
    """Persistent flock inode: never unlink a lock another process may hold."""
    physical(directory, True, True)
    path = directory / "owner.lock"
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, 0o600)
    try:
        physical(path, True)
        require(identity(os.fstat(fd)) == identity(path.lstat()), "owner lock changed")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("renewal owner already active") from error
        yield
    finally:
        os.close(fd)


def group_absent(pid):
    try:
        os.killpg(pid, 0)
        return False
    except ProcessLookupError:
        return True
    except PermissionError:
        return False


def command(argv, cwd, env, timeout, maximum=MAX_OUTPUT):
    """Bound output, deadline and direct process group; retain failure intent.

    TERM permits native XCB/HRA cleanup. Never signal a reaped, reusable PGID.
    A missing native joined receipt is still uncertain even after wrapper exit.
    """
    mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    try:
        child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 start_new_session=True,
                                 preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK, mask))
    except BaseException:
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        raise
    data = bytearray()
    deadline = time.monotonic() + timeout
    reaped = False
    try:
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while selector.get_map():
                require(time.monotonic() < deadline, "command deadline")
                for key, _ in selector.select(0.2):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if chunk:
                        require(len(data) + len(chunk) <= maximum, "command output bound")
                        data.extend(chunk)
                    else:
                        selector.unregister(key.fileobj)
        wait_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        try:
            child.wait(timeout=min(5, max(0.01, deadline - time.monotonic())))
            reaped = True
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, wait_mask)
        require(group_absent(child.pid), "command group remains")
        return child.returncode, bytes(data)
    finally:
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        if not reaped and child.returncode is None:
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                # macOS answers EPERM, not ESRCH, for a group whose leader
                # already exited; either way no live member remains to signal.
                pass
            try:
                child.wait(timeout=60)
            except subprocess.TimeoutExpired:
                # Keep durable intent. A forced exit is never recovery proof.
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
                child.wait(timeout=5)
        child.stdout.close()
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)


def environment(home, scheduler, node, bun):
    # Native tests spawn both runtimes; launchd does not load the user's shell.
    # Exact resolution is checked below, including the scheduler's Bun shebang.
    directories = dict.fromkeys((str(Path(bun).parent), str(Path(node).parent),
                                 str(Path(scheduler).parent), *SYSTEM_PATH.split(":")))
    return {"HOME": home, "PATH": ":".join(directories),
            "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8", "BUN_CONFIG_NO_ENV_FILE": "1",
            "CARGO_HOME": str(Path(home) / ".cargo"), "CARGO_INCREMENTAL": "0"}


def verify_test_runtimes(binding):
    for name in ("bun", "node"):
        selected = physical(Path(binding[name]))
        require(selected.name == name and selected.stat().st_mode & 0o111,
                "native test runtime must be an executable with its standard name")
        found = shutil.which(name, path=binding["environment"]["PATH"])
        require(found is not None and Path(found).resolve(strict=True) == selected,
                "controlled PATH does not select the pinned native test runtime")


def collector_module(path):
    spec = importlib.util.spec_from_file_location("xcb_application_prerequisites", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def source_digest(binding):
    collector = Path(binding["source"]) / "qualification/application-prerequisites.py"
    require(file_hash(collector) == binding["files"][str(collector)], "collector changed")
    # The existing collector owns native input/toolchain enumeration. Its helper
    # uses only Git and toolchain --version here, not Cargo gates or providers.
    previous = dict(os.environ)
    try:
        os.environ.clear()
        os.environ.update(binding["environment"])
        return collector_module(collector).source_fingerprint(Path(binding["source"]), Path(binding["cargo"]))
    finally:
        os.environ.clear()
        os.environ.update(previous)


def generation(binding):
    path = Path(binding["state"]) / "accounts" / binding["account"] / "application-generation.json"
    value = decode(read(path, 1024))
    require(isinstance(value, dict) and set(value) == {"version", "account", "generation"}
            and value["version"] == 1 and value["account"] == binding["account"]
            and digest(value["generation"]), "invalid account generation")
    return value["generation"]


def xcb_argv(binding, *args):
    return [binding["xcb"], "--json", "--state", binding["state"], *args]


def inspect(binding):
    code, raw = command(xcb_argv(binding, "qualify-application", "--inspect", "--account", binding["account"], "--model", binding["model"]),
                        binding["source"], binding["environment"], 30, MAX_JSON)
    require(code == 0, "native inspection unavailable; account/model/doctor pins must be valid")
    value = decode(raw)
    require(isinstance(value, dict) and set(value) == CONTEXT_FIELDS and value["version"] == 1
            and value["provider"] == "claude" and value["os"] == "macos"
            and value["account"] == binding["account"] and value["model"] == binding["model"]
            and value["runtime_sha256"] == binding["files"][binding["xcb"]]
            and value["provider_sha256"] == binding["files"][binding["provider_executable"]], "native inspection binding mismatch")
    return value


def require_conditional_qualifier(binding):
    # Older native binaries have no atomic caller-generation condition. Refuse
    # activation rather than using a post-publication check as that condition.
    code, raw = command([binding["xcb"], "qualify-application", "--help"],
                        binding["source"], binding["environment"], 20, MAX_JSON)
    require(code == 0 and re.search(rb"(?m)^\s+--expected-generation(?:\s|=)", raw) is not None,
            "native qualifier lacks reviewed expected-generation guard; upgrade and requalify before binding")


def capabilities(binding, required=False, *, allow_busy=False):
    # Only completed-renewal verification may ignore a newly acquired lease.
    # This reads native-validated evidence and grants no execution authority.
    require(not allow_busy or required, "busy exemption requires final qualification validation")
    # Capability discovery rehashes exact runtime/provider pins. A bounded
    # allowance covers this CPU work without weakening identity validation.
    code, raw = command(xcb_argv(binding, "generate", "--capabilities"), binding["source"], binding["environment"], 90)
    require(code == 0, "capability read failed")
    value = decode(raw)
    require(value.get("version") == 1 and isinstance(value.get("accounts"), list)
            and len(value["accounts"]) <= 128, "capability response shape")
    rows = [row for row in value["accounts"] if row.get("id") == binding["account"]]
    require(len(rows) == 1 and rows[0].get("provider") == "claude"
            and rows[0].get("enabled") is True and rows[0].get("connected") is True
            and rows[0].get("runtimeAdmitted") is True,
            "selected account missing or unavailable")
    row = rows[0]
    busy = row.get("busy") is True
    if busy and not allow_busy:
        raise AccountBusy("selected native account busy")
    busy_with_evidence = (allow_busy and busy and row.get("available") is False
                          and row.get("reason") == "account_busy")
    qualification = row.get("qualification")
    eligible = busy_with_evidence if busy else row.get("available") is True
    if not eligible or qualification is None:
        require(not required, "bind requires a currently qualified selected account/model")
        return None
    require(any(model.get("key") == binding["model"] for model in row["models"])
            and qualification.get("runtimeDigest") == binding["files"][binding["xcb"]]
            and digest(qualification.get("evidenceDigest"))
            and type(qualification.get("expiresAt")) is int
            and now_ms() < qualification["expiresAt"] <= now_ms() + DAY_MS, "qualification binding or expiry mismatch")
    return qualification


def ambient_files(home):
    return [Path(home) / ".cargo" / name for name in ("config", "config.toml")]


def verify(binding, check_source=True):
    verify_test_runtimes(binding)
    require(str(Path(binding["scheduler"]).resolve(strict=True)) == binding["scheduler_target"], "scheduler link changed")
    for path, wanted in binding["files"].items():
        require(file_hash(Path(path)) == wanted, "pinned executable/source changed; explicit rebind required")
    for path, wanted in binding["ambient"].items():
        require((file_hash(Path(path)) if Path(path).exists() or Path(path).is_symlink() else None) == wanted,
                "ambient Cargo configuration changed; explicit rebind required")
    physical(Path(binding["state"]), True, True)
    require(generation(binding) == binding["generation"], "account sign-in changed; explicit rebind required")
    if check_source:
        require(source_digest(binding) == binding["source_sha256"], "native source/toolchain changed; explicit rebind required")


def bind(args):
    require(sys.platform == "darwin", "renewal supports macOS Claude only")
    require(re.fullmatch(r"a_[A-Za-z0-9_]{1,100}", args.account) is not None
            and re.fullmatch(r"claude/[A-Za-z0-9._/-]{1,200}", args.model) is not None, "account/model shape")
    home = str(Path.home().resolve(strict=True))
    paths = {key: str(getattr(args, key).absolute()) for key in ("source", "state", "xcb", "provider_executable", "scheduler", "cargo", "node", "bun")}
    for key, path in paths.items():
        if key == "scheduler":
            # Invoke the installed public wrapper path; pin its resolved bytes.
            require(Path(path).is_absolute(), "scheduler path")
            physical(Path(path).resolve(strict=True))
        else:
            physical(Path(path), key == "state", key in ("source", "state"))
    script = str(Path(__file__).resolve(strict=True))
    collector = str(Path(paths["source"]) / "qualification/application-prerequisites.py")
    files = [paths["xcb"], paths["provider_executable"], paths["cargo"], paths["node"], paths["bun"], script, collector,
             str(Path(paths["source"]) / "qualification/claude-kernel.py"), str(Path(paths["scheduler"]).resolve(strict=True)),
             str(Path(sys.executable).resolve(strict=True)), str(Path("/opt/homebrew/bin/rustc").resolve(strict=True))]
    binding = {"schema": SCHEMA, **paths, "script": script, "python": str(Path(sys.executable).resolve(strict=True)),
               "scheduler_target": str(Path(paths["scheduler"]).resolve(strict=True)),
               "home": home, "account": args.account, "model": args.model,
               "files": {path: file_hash(Path(path)) for path in files},
               "ambient": {str(path): file_hash(path) if path.exists() or path.is_symlink() else None for path in ambient_files(home)},
               "environment": environment(home, paths["scheduler"], paths["node"], paths["bun"]), "created_at_ms": now_ms()}
    verify_test_runtimes(binding)
    binding["generation"] = generation(binding)
    require_conditional_qualifier(binding)
    binding["context"] = inspect(binding)
    binding["source_sha256"] = source_digest(binding)
    capabilities(binding, True)
    verify(binding)
    directory = private_directory(args.directory.absolute())
    private_directory(directory / "attempts")
    write_once(directory / "binding.json", encoded(binding))
    print(json.dumps({"status": "bound", "scheduled": False, "directory": str(directory)}))


FIELDS = {"schema", "source", "state", "xcb", "provider_executable", "scheduler", "scheduler_target", "cargo", "script", "python", "node", "bun", "home", "account", "model", "files", "ambient", "environment", "created_at_ms", "generation", "context", "source_sha256"}


def load(directory):
    physical(directory, True, True)
    binding = decode(read(directory / "binding.json"))
    require(isinstance(binding, dict) and set(binding) == FIELDS and binding["schema"] == SCHEMA,
            "unsupported binding")
    require(re.fullmatch(r"a_[A-Za-z0-9_]{1,100}", binding["account"]) is not None
            and re.fullmatch(r"claude/[A-Za-z0-9._/-]{1,200}", binding["model"]) is not None
            and digest(binding["generation"]), "account/model/generation shape")
    require(binding["script"] == str(Path(__file__).resolve(strict=True))
            and binding["python"] == str(Path(sys.executable).resolve(strict=True))
            and binding["environment"] == environment(binding["home"], binding["scheduler"], binding["node"], binding["bun"])
            and binding["home"] == str(Path.home().resolve(strict=True)), "runner/home/environment changed")
    required_files = {binding[key] for key in ("xcb", "provider_executable", "script", "python", "cargo", "scheduler_target", "node", "bun")}
    required_files.update(str(Path(binding["source"]) / "qualification" / name)
                          for name in ("application-prerequisites.py", "claude-kernel.py"))
    required_files.add(str(Path("/opt/homebrew/bin/rustc").resolve(strict=True)))
    require(isinstance(binding["files"], dict) and set(binding["files"]) == required_files
            and all(digest(x) for x in binding["files"].values()) and digest(binding["source_sha256"]), "incomplete pins")
    require(set(binding["ambient"]) == {str(path) for path in ambient_files(binding["home"])}, "ambient config pins")
    return binding


def scheduled(binding, mode, label, argv):
    return [binding["scheduler"], "--mode=" + mode, "--lane=mac-native", "--label=" + label, "--", *argv]


def collect_argv(binding, evidence):
    return scheduled(binding, "exclusive", "xcb-application-renewal-prerequisites", [binding["python"], "-I",
        str(Path(binding["source"]) / "qualification/application-prerequisites.py"), "collect", "--xcb", binding["xcb"],
        "--state", binding["state"], "--source", binding["source"], "--provider", "claude", "--account", binding["account"],
        "--model", binding["model"], "--provider-executable", binding["provider_executable"], "--cargo", binding["cargo"], "--output", str(evidence)])


def evidence_budget(directory):
    root = physical(directory / "attempts", True, True)
    entries = list(root.iterdir())
    require(len(entries) < MAX_ATTEMPTS, "renewal evidence attempt budget reached; preserve and review evidence")
    size = 0
    count = 0
    for parent, dirs, names in os.walk(root, followlinks=False):
        physical(Path(parent), True, True)
        for name in dirs:
            physical(Path(parent) / name, True, True)
        for name in names:
            info = physical(Path(parent) / name, True).stat()
            size += info.st_size
            count += 1
            require(size < MAX_EVIDENCE_BYTES and count <= 4096, "renewal evidence byte/file budget reached")


def phase(binding, attempt, name, argv, timeout):
    write_once(attempt / (name + ".intent.json"), encoded({"argv": argv, "started_at_ms": now_ms()}))
    code, output = command(argv, binding["source"], binding["environment"], timeout)
    write_once(attempt / (name + ".output"), output)
    write_once(attempt / (name + ".exit.json"), encoded({"exit_code": code, "finished_at_ms": now_ms(), "output_sha256": sha(output)}))
    require(code == 0, "renewal phase failed: " + name + "; retained intent requires review")
    return output


def run(directory, renew_now=False):
    with owner(directory):
        binding = load(directory)
        verify(binding)
        require_conditional_qualifier(binding)
        require(not (directory / "pending.json").exists(), "previous renewal incomplete; inspect retained evidence and native custody")
        try:
            qualification = capabilities(binding)
        except AccountBusy:
            value = {"status": "deferred", "reason": "native_account_busy"}
            record_status(directory, value)
            print(json.dumps(value))
            return
        if not renew_now and qualification and qualification["expiresAt"] - now_ms() > RENEW_BEFORE_MS:
            value = {"status": "current", "expires_at_ms": qualification["expiresAt"]}
            record_status(directory, value)
            print(json.dumps(value))
            return
        evidence_budget(directory)
        attempt = private_directory(directory / "attempts" / (str(now_ms()) + "-" + uuid.uuid4().hex))
        write_once(directory / "pending.json", encoded({"attempt": attempt.name, "started_at_ms": now_ms()}))
        record_status(directory, {"status": "running", "attempt": attempt.name})
        # Each supported native command acquires the native account lease. Never
        # remove/recover a busy lease or loop a provider operation inside a run.
        phase(binding, attempt, "refresh", scheduled(binding, "shared", "xcb-application-renewal-metadata",
              xcb_argv(binding, "accounts", "refresh", binding["account"])), 300)
        verify(binding)
        require(inspect(binding) == binding["context"], "inspection changed after metadata refresh")
        evidence = attempt / "prerequisites"
        phase(binding, attempt, "collect", collect_argv(binding, evidence), 7200)
        verify(binding)
        require(inspect(binding) == binding["context"], "inspection changed after collection")
        raw = phase(binding, attempt, "qualify", scheduled(binding, "shared", "xcb-application-renewal-live",
                    xcb_argv(binding, "qualify-application", "--account", binding["account"], "--model", binding["model"],
                             EXPECTED_GENERATION_FLAG, binding["generation"], "--evidence", str(evidence))), 600)
        # Scheduler diagnostics share stdout. Native success is independently
        # established through read-only capabilities plus the current receipt.
        verify(binding)
        qualification = capabilities(binding, True, allow_busy=True)
        receipt_bytes = read(Path(binding["state"]) / "qualification/application-v1" / binding["account"] / "receipt.json")
        receipt = decode(receipt_bytes)
        require(sha(receipt_bytes) == qualification["evidenceDigest"]
                and receipt["binding"]["credential_generation"] == binding["generation"]
                and receipt["binding"]["models"] == [binding["model"]]
                and receipt["observed_at_ms"] >= int(attempt.name.split("-")[0])
                and receipt["expires_at_ms"] == qualification["expiresAt"]
                and 0 < receipt["expires_at_ms"] - receipt["observed_at_ms"] <= DAY_MS,
                "renewed receipt does not bind this attempt/account/expiry")
        write_once(attempt / "result.json", encoded({"status": "renewed", "finished_at_ms": now_ms(),
                   "qualification": qualification, "qualifier_output_sha256": sha(raw)}))
        (directory / "pending.json").unlink()
        sync_directory(directory)
        value = {"status": "renewed", "expires_at_ms": qualification["expiresAt"]}
        record_status(directory, value)
        print(json.dumps(value))


def job(binding, directory):
    suffix = sha(encoded([binding["state"], binding["account"], binding["model"]]))[:24]
    label = "dev.xcb.application-renewal." + suffix
    path = Path(binding["home"]) / "Library/LaunchAgents" / (label + ".plist")
    value = {"Label": label, "ProgramArguments": [binding["python"], "-I", binding["script"], "run", "--directory", str(directory)],
             "WorkingDirectory": binding["source"], "EnvironmentVariables": binding["environment"],
             "StartInterval": 3600, "RunAtLoad": False, "ExitTimeOut": 75}
    return label, path, plistlib.dumps(value, sort_keys=True)


def install(directory):
    require(sys.platform == "darwin", "launchd installation requires macOS")
    with owner(directory):
        binding = load(directory)
        verify(binding)
        require_conditional_qualifier(binding)
        capabilities(binding, True)
        label, path, raw = job(binding, directory)
        physical(path.parent, directory=True)
        require(not path.exists() and not path.is_symlink() and not (directory / "launchd.json").exists(),
                "existing launchd installation refused; uninstall the exact owned job first")
        # LaunchAgents need not be private, but this job file itself is0600.
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as file:
            file.write(raw)
            file.flush()
            os.fsync(file.fileno())
        sync_directory(path.parent)
        write_once(directory / "launchd.json", encoded({"label": label, "path": str(path), "sha256": sha(raw)}))
        code, _ = command(["/bin/launchctl", "bootstrap", "gui/" + str(os.getuid()), str(path)], binding["home"], binding["environment"], 20, MAX_JSON)
        require(code == 0, "launchd bootstrap failed; exact plist/ownership retained for inspection")
        code, _ = command(["/bin/launchctl", "print", "gui/" + str(os.getuid()) + "/" + label], binding["home"], binding["environment"], 20, MAX_JSON)
        require(code == 0, "installed launchd job could not be verified")
        print(json.dumps({"status": "installed", "label": label, "run_at_load": False, "interval_seconds": 3600}))


def uninstall(directory):
    require(sys.platform == "darwin", "launchd removal requires macOS")
    with owner(directory):
        binding = load(directory)
        label, path, raw = job(binding, directory)
        receipt = decode(read(directory / "launchd.json"))
        require(receipt == {"label": label, "path": str(path), "sha256": sha(raw)} and read(path) == raw,
                "launchd ownership changed; preserve foreign job")
        code, _ = command(["/bin/launchctl", "bootout", "gui/" + str(os.getuid()), str(path)], binding["home"], binding["environment"], 90, MAX_JSON)
        require(code == 0, "launchd bootout not proven; preserve exact job and evidence")
        require(read(path) == raw, "launchd plist changed during removal")
        path.unlink()
        (directory / "launchd.json").unlink()
        sync_directory(path.parent)
        sync_directory(directory)
        print(json.dumps({"status": "uninstalled", "evidence_preserved": True}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    binder = sub.add_parser("bind", help="pin a currently qualified exact selection; no schedule/provider request")
    for name in ("directory", "source", "state", "xcb", "provider-executable", "scheduler", "cargo", "node", "bun"):
        binder.add_argument("--" + name, type=Path, required=True)
    binder.add_argument("--account", required=True)
    binder.add_argument("--model", required=True)
    runner = sub.add_parser("run")
    runner.add_argument("--directory", type=Path, required=True)
    runner.add_argument("--renew-now", action="store_true", help="explicitly collect fresh evidence now, preserving all gates and native expiry")
    for action in ("install", "uninstall"):
        sub.add_parser(action).add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        if args.action == "bind":
            bind(args)
        elif args.action == "run":
            run(args.directory.absolute(), args.renew_now)
        else:
            globals()[args.action](args.directory.absolute())
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError, UnicodeError):
        # Raw provider/host diagnostics remain private. No retry or recovery.
        value = {"status": "stopped", "action": args.action,
                 "reason": "renewal gate failed; inspect private binding and attempt evidence"}
        if args.action == "run":
            try:
                with owner(args.directory.absolute()):
                    load(args.directory.absolute())
                    record_status(args.directory.absolute(), value)
            except (ValueError, OSError, KeyError, TypeError):
                pass
        print(json.dumps(value), file=sys.stderr)
        return 1


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(InterruptedError("terminated")))
    signal.signal(signal.SIGINT, lambda *_: (_ for _ in ()).throw(InterruptedError("interrupted")))
    sys.exit(main())
