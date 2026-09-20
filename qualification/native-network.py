#!/usr/bin/env python3
"""Credential-free macOS DNS/TLS regression for the native provider policies."""
import argparse
import codecs
import ctypes
import datetime
import encodings.idna
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time

GRANT = '(allow file-read-metadata (literal "/var"))\n'
TARGETS = {
    "codex": ("chatgpt.com", "/backend-api/wham/usage"),
    "devin": ("server.codeium.com", "/"),
}
LIMIT = 65536
DEADLINE = 25


def sha(data):
    return hashlib.sha256(data).hexdigest()


def child(provider, policy_path):
    # Resolve Python's lazy imports and public trust store before confinement.
    # The probe measures network policy, not Python installation read access.
    for encoding in ("idna", "ascii", "utf-8", "latin-1"):
        codecs.lookup(encoding)
    context = ssl.create_default_context(cafile="/private/etc/ssl/cert.pem")
    policy = Path(policy_path).read_bytes()
    if len(policy) > LIMIT:
        raise ValueError("policy bound")
    library = ctypes.CDLL("/usr/lib/libsandbox.1.dylib", use_errno=True)
    library.sandbox_init.argtypes = [ctypes.c_char_p, ctypes.c_uint64,
                                    ctypes.POINTER(ctypes.c_char_p)]
    library.sandbox_init.restype = ctypes.c_int
    error = ctypes.c_char_p()
    result = {"dns": False, "tcp": False, "tls": False, "http_status": None,
              "python_version": platform.python_version()}
    started = time.monotonic()
    if library.sandbox_init(policy, 0, ctypes.byref(error)) != 0:
        result["failure"] = "sandbox initialization"
    else:
        host, path = TARGETS[provider]
        try:
            addresses = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
            if not addresses:
                raise socket.gaierror()
            result["dns"] = True
            with socket.create_connection((host, 443), timeout=8) as connection:
                result["tcp"] = True
                with context.wrap_socket(connection, server_hostname=host) as secured:
                    result["tls"] = True
                    request = ("GET " + path + " HTTP/1.1\r\nHost: " + host
                               + "\r\nConnection: close\r\n\r\n").encode("ascii")
                    secured.sendall(request)
                    header = bytearray()
                    while b"\r\n" not in header and len(header) < 1024:
                        chunk = secured.recv(1024 - len(header))
                        if not chunk:
                            break
                        header.extend(chunk)
                    status = bytes(header).split(b"\r\n", 1)[0]
                    match = re.fullmatch(rb"HTTP/1\.[01] ([1-5][0-9]{2})(?: .*)?", status)
                    if match is None:
                        raise ValueError("HTTP status")
                    result["http_status"] = int(match.group(1))
        except Exception as exception:
            # Never serialize error text, response bytes, addresses, or paths.
            categories = {socket.gaierror: "DNS", ssl.SSLError: "TLS",
                          TimeoutError: "timeout", ValueError: "HTTP status"}
            result["failure"] = next((name for cls, name in categories.items()
                                      if isinstance(exception, cls)), "network")
    result["elapsed_seconds"] = round(time.monotonic() - started, 3)
    print(json.dumps(result, separators=(",", ":")), flush=True)


def template(source, provider):
    start = source.index("pub fn " + provider + "_seatbelt(")
    remainder = source[start:]
    end = remainder.index('\n}\n') + 3
    function = remainder[:end]
    matches = re.findall(r'r#"(.*?)"#', function, flags=re.S)
    if len(matches) != 1 or matches[0].count(GRANT) != 1:
        raise ValueError("production template or exact /var grant changed")
    expected = ({"exe", "work", "profile", "config", "catalog", "ca_bundle"}
                if provider == "codex" else
                {"exe", "helper", "work", "home", "config", "config_parent", "network"})
    if set(re.findall(r"\{([^{}]+)\}", matches[0])) != expected:
        raise ValueError("production template parameters changed")
    return matches[0], sha(function.encode())


def policy_for(raw, directory):
    paths = {"exe": directory / "provider", "helper": directory / "helper",
             "work": directory / "scratch", "profile": directory / "scratch/profile",
             "home": directory / "scratch/home", "catalog": directory / "catalog",
             "ca_bundle": directory / "public-ca.pem"}
    paths["config"] = (paths["profile"] / "config.toml" if "{profile}" in raw
                       else paths["home"] / ".config/devin")
    paths["config_parent"] = paths["config"].parent
    for name, path in paths.items():
        raw = raw.replace("{" + name + "}", json.dumps(str(path)))
    # Metadata-only Devin session/new has no broker socket, hence socket=None.
    return raw.replace("{network}", "")


def group_absent(pid):
    try:
        os.killpg(pid, 0)
        return False
    except ProcessLookupError:
        return True
    except PermissionError:
        # A zombie-only macOS group may be unsignalable; this is not absence.
        return False


def run_child(script, provider, policy_path, directory):
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    try:
        process = subprocess.Popen(
        ["/usr/bin/python3", "-I", str(script), "--child", provider, str(policy_path)],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        cwd=directory, env={"HOME": str(directory), "TMPDIR": str(directory),
                            "PATH": "/usr/bin:/bin", "LANG": "en_US.UTF-8"},
        start_new_session=True)
    except BaseException:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        raise
    capture = {"stdout": bytearray(), "stderr": bytearray()}
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
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError()
                for key, _ in selector.select(min(remaining, 0.25)):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    if len(capture[key.data]) + len(chunk) > LIMIT:
                        raise OverflowError()
                    capture[key.data].extend(chunk)
            stdio_joined = True
            # Keep the leader unreaped until group cleanup: its PID must not be reused.
    except (Exception, KeyboardInterrupt) as exception:
        problem = "timeout" if isinstance(exception, TimeoutError) else "capture or interruption"
    finally:
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        # Stop and join every surviving member, including on parent SIGTERM.
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
    row = {"exit_code": process.returncode, "group_joined": joined, "stdio_joined": stdio_joined,
           "stderr_sha256": sha(capture["stderr"])}
    if problem:
        row["failure"] = problem
    if not joined:
        raise RuntimeError("unproven child group exit; no later scenario may run")
    try:
        payload = json.loads(capture["stdout"])
        keys = {"dns", "tcp", "tls", "http_status", "elapsed_seconds", "failure",
                "python_version"}
        if not isinstance(payload, dict) or not set(payload) <= keys:
            raise ValueError()
        # Closed child schema prevents accidental public receipt expansion.
        if any(type(payload.get(key)) is not bool for key in ("dns", "tcp", "tls")):
            raise ValueError()
        if payload.get("failure") not in (None, "DNS", "TLS", "timeout", "HTTP status",
                                          "network", "sandbox initialization"):
            raise ValueError()
        version = payload.get("python_version")
        if not isinstance(version, str) or re.fullmatch(r"[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}", version) is None:
            raise ValueError()
        status = payload.get("http_status")
        if status is not None and (type(status) is not int or not 100 <= status <= 599):
            raise ValueError()
        elapsed = payload.get("elapsed_seconds")
        if type(elapsed) not in (int, float) or not 0 <= elapsed <= DEADLINE + 1:
            raise ValueError()
        row["network"] = payload
    except (ValueError, TypeError):
        row["failure"] = "invalid child result"
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS is required; no unsandboxed fallback")
    script = Path(__file__).resolve()
    source_path = script.parent.parent / "crates/xcb-runtime/src/sandbox.rs"
    source_bytes = source_path.read_bytes()
    if len(source_bytes) > 1048576:
        raise ValueError("sandbox source bound")
    script_hash = sha(script.read_bytes())
    source = source_bytes.decode("utf-8")
    rows = []
    directory = Path(tempfile.mkdtemp(prefix="xcb-native-network-")).resolve()
    directory.chmod(0o700)
    completed = False
    try:
        for provider in TARGETS:
            raw, function_hash = template(source, provider)
            production = policy_for(raw, directory)
            for variant, policy in (("without_var_metadata", production.replace(GRANT, "")),
                                    ("production", production)):
                policy_path = directory / (provider + "-" + variant + ".sb")
                policy_path.write_text(policy, encoding="utf-8")
                policy_path.chmod(0o600)
                row = run_child(script, provider, policy_path, directory)
                row.update({"provider": provider, "variant": variant,
                            "function_sha256": function_hash,
                            "template_sha256": sha(raw.encode()),
                            "policy_sha256": sha(policy.encode())})
                network = row.get("network", {})
                row["passed"] = (row["exit_code"] == 0 and row["group_joined"] and row["stdio_joined"]
                                 and "failure" not in row and
                                 ((variant == "production" and network.get("tls") is True
                                   and network.get("http_status") is not None)
                                  or (variant == "without_var_metadata"
                                      and network.get("dns") is False
                                      and network.get("failure") == "DNS")))
                rows.append(row)
        completed = True
    finally:
        # Keep scratch state if custody failed; never delete beneath a live child.
        if completed:
            shutil.rmtree(directory)
    unchanged = source_path.read_bytes() == source_bytes and sha(script.read_bytes()) == script_hash
    receipt = {"schema_version": 1, "credential_free": True, "metadata_only": True,
               "live_provider_qualification": False,
               "scope": "Python DNS/TCP/TLS under exact native metadata-session policy templates",
               "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "platform": sys.platform, "architecture": platform.machine(),
               "macos_version": platform.mac_ver()[0],
               "controller_python_version": platform.python_version(),
               "probe_python_versions": sorted({row["network"]["python_version"] for row in rows
                                                 if "network" in row}),
               "harness_sha256": script_hash, "sandbox_source_sha256": sha(source_bytes),
               "source_unchanged": unchanged, "cases": rows,
               "passed": unchanged and len(rows) == 4 and all(row["passed"] for row in rows)}
    output = json.dumps(receipt, indent=2, allow_nan=False) + "\n"
    args.output.write_text(output, encoding="utf-8")
    print(output, end="")
    return 0 if receipt["passed"] else 1


def interrupted(_signal, _frame):
    raise KeyboardInterrupt()


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    if len(sys.argv) == 4 and sys.argv[1] == "--child" and sys.argv[2] in TARGETS:
        signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM, signal.SIGINT})
        child(sys.argv[2], sys.argv[3])
    else:
        sys.exit(main())
