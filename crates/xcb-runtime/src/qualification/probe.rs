//! Native Linux sandbox test (`xcb doctor --qualify-sandbox`): three
//! credential-free probes run through the same bwrap planner the Claude
//! launcher uses, then the receipt `sandbox::linux_sandbox` admits.
//!
//! The confined side of every probe is this xcb binary itself (the hidden
//! `sandbox-probe` subcommand), bound with its `ldd` closure exactly the way
//! the launcher binds the in-namespace forwarder. No compiler, Bun, or source
//! checkout is involved, no account or model is touched, and nothing ever
//! runs unsandboxed: a host where bwrap can't start simply fails the probe.
//!
//! - `linux-sandbox`: foreign paths absent, own scratch writable, a fresh
//!   network namespace (a live host loopback listener is unreachable), and
//!   a fresh PID namespace.
//! - `linux-egress`: the host CONNECT bridge socket is reachable and tunnels
//!   bytes both ways, while direct TCP and unbound unix paths stay absent.
//! - `linux-loopback`: the launcher's own plan (`runner::linux_spec`) with
//!   the in-namespace forwarder: a child given only `HTTPS_PROXY` reaches a
//!   synthetic upstream through forwarder → bridge, and receives the
//!   forwarder's private env file.

use super::{
    EXPECTED_PROBES, LINUX_QUALIFICATION_NAME, LINUX_QUALIFICATION_SCHEMA, LinuxQualification,
    Namespaces, Probe, Wrapper,
};
use crate::{Error, Result, egress, sandbox};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The hidden CLI subcommand that runs the confined half of a probe.
pub const PROBE_SUBCOMMAND: &str = "sandbox-probe";

const PROBE_HOST: &str = "probe.invalid";
const PROBE_TOKEN: &[u8] = b"PROBE-TOKEN";
const PROBE_PING: &[u8] = b"PING";
const CANARY: &str = "synthetic-private-canary";
const ENV_KEY: &str = "XCB_SANDBOX_TEST";
const ENV_VALUE: &str = "env-file";
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// One probe's evidence: the receipt verdict plus what was observed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeEvidence {
    pub name: &'static str,
    pub passed: bool,
    pub exit_code: i32,
    /// The confined side's self-report (last stdout line, JSON).
    pub observed: Value,
    /// Host-side checks: canary, bridge shutdown, upstream bytes.
    pub host: Value,
    pub policy_sha256: Option<String>,
    /// bwrap or child stderr, bounded; empty on a clean run.
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationRun {
    pub receipt_path: PathBuf,
    pub receipt: LinuxQualification,
    pub probes: Vec<ProbeEvidence>,
    pub qualified: bool,
}

/// Run the three probes and write `<root>/qualification/linux.json`.
///
/// The receipt is written whatever the outcome — evidence first — and a
/// failed probe is recorded as failed, so admission refuses it and doctor can
/// say which probe failed. An error means no probe could be planned at all.
pub async fn qualify(root: &Path) -> Result<QualificationRun> {
    if !cfg!(target_os = "linux") {
        return Err(Error::Unavailable("the sandbox test runs on Linux only"));
    }
    let candidate =
        sandbox::bwrap_candidate().ok_or(Error::Unavailable("bwrap isn't installed"))?;
    let pin = sandbox::BwrapPin::admit(&candidate)?;
    let xcb = xcb_core::canonical(std::env::current_exe()?)?;
    let closure = sandbox::shared_library_closure(&xcb)?;
    let namespaces = Namespaces::live();
    let work = tempfile::Builder::new()
        .prefix("xcb-sandbox-test-")
        .tempdir()?;
    let base = xcb_core::canonical(work.path())?;
    let context = Context {
        pin: &pin,
        xcb: &xcb,
        closure: &closure,
        base: &base,
    };
    let probes = vec![
        context.isolation().await,
        context.egress().await,
        context.loopback().await,
    ];
    drop(work);
    let receipt = LinuxQualification {
        schema: LINUX_QUALIFICATION_SCHEMA.into(),
        wrapper: Wrapper {
            path: candidate.clone(),
            sha256: pin.sha256.clone(),
        },
        namespaces,
        probes: probes
            .iter()
            .map(|probe| {
                (
                    probe.name.to_owned(),
                    Probe {
                        exit_code: probe.exit_code,
                        passed: Some(probe.passed),
                    },
                )
            })
            .collect(),
        observed_at_ms: crate::now_ms(),
    };
    let receipt_path = write_receipt(root, &receipt)?;
    let qualified = receipt.qualified(
        &candidate,
        &pin.sha256,
        &Namespaces::live(),
        crate::now_ms(),
    );
    Ok(QualificationRun {
        receipt_path,
        receipt,
        probes,
        qualified,
    })
}

fn write_receipt(root: &Path, receipt: &LinuxQualification) -> Result<PathBuf> {
    let root = crate::private::directory(root)?;
    let directory = crate::private::directory(&root.join("qualification"))?;
    let path = root.join(LINUX_QUALIFICATION_NAME);
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    // NamedTempFile is 0600; persist renames over any earlier receipt.
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(&path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(path)
}

struct Context<'a> {
    pin: &'a sandbox::BwrapPin,
    xcb: &'a Path,
    closure: &'a [PathBuf],
    base: &'a Path,
}

struct Ran {
    code: i32,
    observed: Value,
    stderr: String,
    policy_sha256: Option<String>,
}

impl Context<'_> {
    fn directory(&self, name: &str) -> Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt;
        let path = self.base.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(path)
    }

    fn spec(&self, scratch: &Path, policy: PathBuf, socket: Option<PathBuf>) -> sandbox::BwrapSpec {
        sandbox::BwrapSpec {
            executable: self.xcb.to_owned(),
            scratch: scratch.to_owned(),
            account_home: None,
            read_only: self.closure.to_vec(),
            egress: if socket.is_some() {
                sandbox::Egress::Tcp443Dns
            } else {
                sandbox::Egress::Denied
            },
            socket,
            forwarder: None,
            policy_path: policy,
        }
    }

    /// Plan and run one confined probe; planning failures become a failed
    /// run with the reason, never an unsandboxed fallback.
    async fn run(
        &self,
        spec: &sandbox::BwrapSpec,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Ran {
        let launch = match sandbox::bwrap_launch(self.pin, spec, args, env, &spec.scratch) {
            Ok(launch) => launch,
            Err(error) => {
                return Ran {
                    code: -1,
                    observed: Value::Null,
                    stderr: format!("sandbox plan refused: {error}"),
                    policy_sha256: None,
                };
            }
        };
        let mut command = tokio::process::Command::new(&launch.executable);
        command
            .args(&launch.args)
            .env_clear()
            .envs(&launch.env)
            .current_dir(&spec.scratch)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let policy_sha256 = Some(launch.policy_sha256);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return Ran {
                    code: -1,
                    observed: Value::Null,
                    stderr: format!("bwrap could not start: {error}"),
                    policy_sha256,
                };
            }
        };
        match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => Ran {
                code: output.status.code().unwrap_or(-1),
                observed: last_json_line(&output.stdout),
                stderr: bounded(&output.stderr),
                policy_sha256,
            },
            Ok(Err(error)) => Ran {
                code: -1,
                observed: Value::Null,
                stderr: format!("probe wait failed: {error}"),
                policy_sha256,
            },
            Err(_) => Ran {
                code: -1,
                observed: Value::Null,
                stderr: "probe timed out".into(),
                policy_sha256,
            },
        }
    }

    async fn isolation(&self) -> ProbeEvidence {
        let name = EXPECTED_PROBES[0];
        let prepared = (|| -> Result<_> {
            let dir = self.directory("sandbox")?;
            let scratch = self.directory("sandbox/scratch")?;
            let foreign = self.directory("sandbox/foreign")?;
            let canary = foreign.join("canary");
            std::fs::write(&canary, CANARY)?;
            // A live host listener: reaching it would mean the network
            // namespace is shared.
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            Ok((dir, scratch, foreign, canary, listener))
        })();
        let (dir, scratch, foreign, canary, listener) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return failed(name, error),
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let spec = self.spec(&scratch, dir.join("policy.json"), None);
        let args = probe_args(
            "isolation",
            &[
                &scratch.join("own"),
                &canary,
                &foreign.join("write"),
                Path::new(&port.to_string()),
                &foreign,
            ],
        );
        let env = inner_env(&scratch);
        let ran = self.run(&spec, &args, &env).await;
        drop(listener);
        let canary_intact = std::fs::read_to_string(&canary).is_ok_and(|text| text == CANARY);
        let foreign_untouched = !foreign.join("write").exists();
        let host = json!({"canaryIntact": canary_intact, "foreignUntouched": foreign_untouched});
        let passed = ran.code == 0
            && all_true(&ran.observed, ISOLATION_FIELDS)
            && canary_intact
            && foreign_untouched;
        evidence(name, passed, ran, host)
    }

    async fn egress(&self) -> ProbeEvidence {
        let name = EXPECTED_PROBES[1];
        let prepared = async {
            let dir = self.directory("egress")?;
            let scratch = self.directory("egress/scratch")?;
            let run = self.directory("egress/run")?;
            let upstream = Upstream::start().await?;
            let bridge = upstream.bridge(run.join("egress.sock")).await?;
            Ok::<_, Error>((dir, scratch, run, upstream, bridge))
        }
        .await;
        let (dir, scratch, run, upstream, bridge) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return failed(name, error),
        };
        let socket = bridge.socket_path().to_owned();
        let spec = self.spec(&scratch, dir.join("policy.json"), Some(socket.clone()));
        let args = probe_args("egress", &[&socket, &run.join("absent.sock")]);
        let ran = self.run(&spec, &args, &inner_env(&scratch)).await;
        let closed = bridge.close().await;
        let host = json!({
            "upstreamSawClientBytes": upstream.saw_ping(),
            "bridge": closed,
        });
        let passed = ran.code == 0
            && all_true(&ran.observed, EGRESS_FIELDS)
            && upstream.saw_ping()
            && closed.listener_closed
            && closed.sockets_joined
            && closed.socket_removed;
        evidence(name, passed, ran, host)
    }

    async fn loopback(&self) -> ProbeEvidence {
        let name = EXPECTED_PROBES[2];
        let prepared = async {
            let dir = self.directory("loopback")?;
            let scratch = self.directory("loopback/scratch")?;
            let run = self.directory("loopback/run")?;
            let env_file = egress::write_forwarder_env(
                &scratch,
                &BTreeMap::from([(ENV_KEY.to_owned(), ENV_VALUE.to_owned())]),
            )?;
            let upstream = Upstream::start().await?;
            let bridge = upstream.bridge(run.join("egress.sock")).await?;
            Ok::<_, Error>((dir, scratch, env_file, upstream, bridge))
        }
        .await;
        let (dir, scratch, env_file, upstream, bridge) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return failed(name, error),
        };
        // Exactly the launcher's plan, with this binary standing in for the
        // provider snapshot.
        let spec = crate::runner::linux_spec(
            self.xcb.to_owned(),
            self.xcb.to_owned(),
            scratch.clone(),
            dir.join("policy.json"),
            bridge.socket_path().to_owned(),
            env_file.clone(),
            self.closure.to_vec(),
        );
        let args = probe_args("loopback", &[]);
        let wrapper_env = BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        let ran = self.run(&spec, &args, &wrapper_env).await;
        let closed = bridge.close().await;
        let env_file_consumed = !env_file.exists();
        let host = json!({
            "upstreamSawClientBytes": upstream.saw_ping(),
            "envFileConsumed": env_file_consumed,
            "bridge": closed,
        });
        let passed = ran.code == 0
            && all_true(&ran.observed, LOOPBACK_FIELDS)
            && upstream.saw_ping()
            && env_file_consumed
            && closed.listener_closed
            && closed.sockets_joined
            && closed.socket_removed;
        evidence(name, passed, ran, host)
    }
}

fn probe_args(kind: &str, paths: &[&Path]) -> Vec<String> {
    [PROBE_SUBCOMMAND, kind]
        .into_iter()
        .map(str::to_owned)
        .chain(paths.iter().map(|path| path.to_string_lossy().into_owned()))
        .collect()
}

fn inner_env(scratch: &Path) -> BTreeMap<String, String> {
    let scratch = scratch.to_string_lossy().into_owned();
    BTreeMap::from([
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("HOME".to_owned(), scratch.clone()),
        ("TMPDIR".to_owned(), scratch),
    ])
}

fn evidence(name: &'static str, passed: bool, ran: Ran, host: Value) -> ProbeEvidence {
    ProbeEvidence {
        name,
        passed,
        exit_code: ran.code,
        observed: ran.observed,
        host,
        policy_sha256: ran.policy_sha256,
        stderr: ran.stderr,
    }
}

fn failed(name: &'static str, error: Error) -> ProbeEvidence {
    ProbeEvidence {
        name,
        passed: false,
        exit_code: -1,
        observed: Value::Null,
        host: Value::Null,
        policy_sha256: None,
        stderr: format!("probe setup failed: {error}"),
    }
}

fn bounded(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let mut end = text.len().min(2048);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn last_json_line(stdout: &[u8]) -> Value {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or(Value::Null)
}

fn all_true(observed: &Value, fields: &[&str]) -> bool {
    fields
        .iter()
        .all(|field| observed.get(*field) == Some(&Value::Bool(true)))
}

const ISOLATION_FIELDS: &[&str] = &[
    "ownWrite",
    "foreignReadDenied",
    "foreignWriteDenied",
    "foreignDirDenied",
    "netDenied",
    "pidIsolated",
];
const SEATBELT_FIELDS: &[&str] = &[
    "ownWrite",
    "foreignReadDenied",
    "foreignWriteDenied",
    "foreignDirDenied",
    "netDenied",
];

const EGRESS_FIELDS: &[&str] = &[
    "bridgeConnect",
    "connectEstablished",
    "tunneledReply",
    "absentSocketDenied",
    "directTcpDenied",
    "pidIsolated",
];
const LOOPBACK_FIELDS: &[&str] = &[
    "proxyConnect",
    "connectEstablished",
    "tunneledReply",
    "envInjected",
    "directTcpDenied",
];

/// A synthetic TLS-free upstream behind the bridge: it answers the first
/// client bytes with a fixed token. The CONNECT tunnel is byte-transparent,
/// so no certificate or real endpoint is needed.
struct Upstream {
    address: std::net::SocketAddr,
    saw_ping: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Upstream {
    async fn start() -> Result<Self> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let saw_ping = Arc::new(AtomicBool::new(false));
        let flag = saw_ping.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let flag = flag.clone();
                tokio::spawn(async move {
                    let mut received = Vec::new();
                    let mut buffer = [0u8; 256];
                    let _ = tokio::time::timeout(IO_TIMEOUT, async {
                        while !contains(&received, PROBE_PING) && received.len() < 4096 {
                            match stream.read(&mut buffer).await {
                                Ok(0) | Err(_) => return,
                                Ok(count) => received.extend_from_slice(&buffer[..count]),
                            }
                        }
                        flag.store(true, Ordering::SeqCst);
                        let _ = stream.write_all(PROBE_TOKEN).await;
                        // Hold the connection until the client closes it.
                        let _ = stream.read(&mut buffer).await;
                    })
                    .await;
                });
            }
        });
        Ok(Self {
            address,
            saw_ping,
            task,
        })
    }

    async fn bridge(&self, socket: PathBuf) -> Result<egress::EgressBridge> {
        let address = self.address;
        let mut options = egress::EgressBridgeOptions::new(socket);
        options.allowlist = Some(BTreeSet::from([PROBE_HOST.to_owned()]));
        options.dialer = Some(Arc::new(move |_host, _port| {
            Box::pin(async move { Ok(tokio::net::TcpStream::connect(address).await?) })
        }));
        egress::EgressBridge::start(options).await
    }

    fn saw_ping(&self) -> bool {
        self.saw_ping.load(Ordering::SeqCst)
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The confined half: `xcb sandbox-probe <kind> <args…>`. Prints one JSON
/// line describing what held inside the namespace and returns the exit code
/// (0 only when every assertion held).
pub fn inside(args: &[String]) -> i32 {
    let (report, fields): (Value, &[&str]) = match args {
        [kind, own, canary, foreign_write, port, foreign_dir] if kind == "isolation" => (
            isolation(own, canary, foreign_write, port, foreign_dir),
            ISOLATION_FIELDS,
        ),
        [kind, socket, absent] if kind == "egress" => {
            (egress_inside(socket, absent), EGRESS_FIELDS)
        }
        [kind] if kind == "loopback" => (loopback_inside(), LOOPBACK_FIELDS),
        [kind, own, canary, foreign_write, port, foreign_dir] if kind == "seatbelt" => (
            seatbelt_inside(own, canary, foreign_write, port, foreign_dir),
            SEATBELT_FIELDS,
        ),
        _ => {
            eprintln!("sandbox-probe: unknown probe");
            return 2;
        }
    };
    println!("{report}");
    i32::from(!all_true(&report, fields))
}

fn isolation(own: &str, canary: &str, foreign_write: &str, port: &str, foreign_dir: &str) -> Value {
    let own_write = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(own)
        .and_then(|mut file| file.write_all(b"own"))
        .is_ok();
    let foreign_read_denied = std::fs::File::open(canary).is_err();
    let foreign_write_denied = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(foreign_write)
        .is_err();
    let foreign_dir_denied = std::fs::read_dir(foreign_dir).is_err();
    let net_denied = port
        .parse::<u16>()
        .map(|port| {
            std::net::TcpStream::connect_timeout(&(([127, 0, 0, 1], port).into()), IO_TIMEOUT)
                .is_err()
        })
        .unwrap_or(false);
    json!({
        "ownWrite": own_write,
        "foreignReadDenied": foreign_read_denied,
        "foreignWriteDenied": foreign_write_denied,
        "foreignDirDenied": foreign_dir_denied,
        "netDenied": net_denied,
        "pidIsolated": pid_isolated(),
    })
}

/// macOS has no PID or network namespace: the seatbelt half checks file
/// confinement and that a live host loopback listener is unreachable.
fn seatbelt_inside(
    own: &str,
    canary: &str,
    foreign_write: &str,
    port: &str,
    foreign_dir: &str,
) -> Value {
    let mut report = isolation(own, canary, foreign_write, port, foreign_dir);
    if let Some(object) = report.as_object_mut() {
        object.remove("pidIsolated");
    }
    report
}

fn egress_inside(socket: &str, absent: &str) -> Value {
    let (connected, established, tunneled) = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(stream) => {
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            let (established, tunneled) = tunnel(stream);
            (true, established, tunneled)
        }
        Err(_) => (false, false, false),
    };
    json!({
        "bridgeConnect": connected,
        "connectEstablished": established,
        "tunneledReply": tunneled,
        "absentSocketDenied": std::os::unix::net::UnixStream::connect(absent).is_err(),
        "directTcpDenied": direct_tcp_denied(),
        "pidIsolated": pid_isolated(),
    })
}

fn loopback_inside() -> Value {
    let proxy = std::env::var("HTTPS_PROXY").unwrap_or_default();
    let address = proxy
        .strip_prefix("http://")
        .unwrap_or(&proxy)
        .trim_end_matches('/')
        .parse::<std::net::SocketAddr>()
        .ok();
    let (connected, established, tunneled) = match address
        .and_then(|address| std::net::TcpStream::connect_timeout(&address, IO_TIMEOUT).ok())
    {
        Some(stream) => {
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            let (established, tunneled) = tunnel(stream);
            (true, established, tunneled)
        }
        None => (false, false, false),
    };
    json!({
        "proxyConnect": connected,
        "connectEstablished": established,
        "tunneledReply": tunneled,
        "envInjected": std::env::var(ENV_KEY).is_ok_and(|value| value == ENV_VALUE),
        "directTcpDenied": direct_tcp_denied(),
    })
}

/// CONNECT to the probe host, then send client bytes and wait for the
/// upstream's token. Returns (200 received, token received).
fn tunnel(mut stream: impl Read + Write) -> (bool, bool) {
    let request = format!("CONNECT {PROBE_HOST}:443 HTTP/1.1\r\nHost: {PROBE_HOST}:443\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return (false, false);
    }
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) if head.len() < 4096 => head.push(byte[0]),
            _ => return (false, false),
        }
    }
    let established = head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200");
    if !established || stream.write_all(PROBE_PING).is_err() {
        return (established, false);
    }
    let mut reply = Vec::new();
    let mut buffer = [0u8; 256];
    while !contains(&reply, PROBE_TOKEN) && reply.len() < 4096 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => reply.extend_from_slice(&buffer[..count]),
        }
    }
    (true, contains(&reply, PROBE_TOKEN))
}

fn direct_tcp_denied() -> bool {
    // TEST-NET-3: never routable; inside a fresh netns there is no route.
    std::net::TcpStream::connect_timeout(&(([203, 0, 113, 1], 443).into()), IO_TIMEOUT).is_err()
}

/// bwrap's --die-with-parent monitor may hold pid 1, so a fresh PID
/// namespace shows the probe at pid 1 or 2, never a host-scale pid.
fn pid_isolated() -> bool {
    std::process::id() <= 2
}

/// The automatic macOS boundary probe behind application admission. This
/// xcb binary runs its confined half under the exact Seatbelt profile the
/// provider launcher generates for `provider`, standing in for the provider
/// executable. It needs no credentials, account, model or provider binary, and
/// a host where `sandbox-exec` can't apply the profile simply fails.
#[cfg(target_os = "macos")]
pub async fn seatbelt(provider: xcb_core::Provider) -> ProbeEvidence {
    const NAME: &str = "macos-seatbelt";
    let prepared = (|| -> Result<_> {
        let xcb = xcb_core::canonical(std::env::current_exe()?)?;
        let work = tempfile::Builder::new()
            .prefix("xcb-seatbelt-test-")
            .tempdir()?;
        let base = xcb_core::canonical(work.path())?;
        let make = |name: &str| -> Result<PathBuf> {
            use std::os::unix::fs::DirBuilderExt;
            let path = base.join(name);
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            Ok(path)
        };
        let scratch = make("scratch")?;
        let foreign = make("foreign")?;
        let canary = foreign.join("canary");
        std::fs::write(&canary, CANARY)?;
        let policy = match provider {
            xcb_core::Provider::Claude => sandbox::seatbelt(&xcb, &scratch)?,
            xcb_core::Provider::Codex => {
                let profile = scratch.join("profile");
                std::fs::create_dir(&profile)?;
                let config = profile.join("config.toml");
                std::fs::write(&config, "")?;
                let catalog = base.join("catalog.json");
                std::fs::write(&catalog, "{}")?;
                let ca_bundle = base.join("ca.pem");
                std::fs::write(&ca_bundle, "")?;
                sandbox::codex_seatbelt(&xcb, &scratch, &profile, &config, &catalog, &ca_bundle)?
            }
            xcb_core::Provider::Devin => {
                return Err(Error::Unavailable("Devin support was removed"));
            }
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        Ok((work, xcb, scratch, foreign, canary, policy, listener))
    })();
    let (work, xcb, scratch, foreign, canary, policy, listener) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return failed(NAME, error),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let policy_sha256 = Some(crate::digest(&policy));
    let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-p")
        .arg(&policy)
        .arg(&xcb)
        .args(probe_args(
            "seatbelt",
            &[
                &scratch.join("own"),
                &canary,
                &foreign.join("write"),
                Path::new(&port.to_string()),
                &foreign,
            ],
        ))
        .env_clear()
        .envs(inner_env(&scratch))
        .current_dir(&scratch)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let ran = match command.spawn() {
        Ok(child) => match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => Ran {
                code: output.status.code().unwrap_or(-1),
                observed: last_json_line(&output.stdout),
                stderr: bounded(&output.stderr),
                policy_sha256,
            },
            Ok(Err(error)) => Ran {
                code: -1,
                observed: Value::Null,
                stderr: format!("probe wait failed: {error}"),
                policy_sha256,
            },
            Err(_) => Ran {
                code: -1,
                observed: Value::Null,
                stderr: "probe timed out".into(),
                policy_sha256,
            },
        },
        Err(error) => Ran {
            code: -1,
            observed: Value::Null,
            stderr: format!("sandbox-exec could not start: {error}"),
            policy_sha256,
        },
    };
    drop(listener);
    let canary_intact = std::fs::read_to_string(&canary).is_ok_and(|text| text == CANARY);
    let foreign_untouched = !foreign.join("write").exists();
    drop(work);
    let host = json!({"canaryIntact": canary_intact, "foreignUntouched": foreign_untouched});
    let passed = ran.code == 0
        && all_true(&ran.observed, SEATBELT_FIELDS)
        && canary_intact
        && foreign_untouched;
    evidence(NAME, passed, ran, host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seatbelt_probe_fails_on_an_unconfined_host() {
        let dir = tempfile::tempdir().unwrap();
        let canary = dir.path().join("canary");
        std::fs::write(&canary, CANARY).unwrap();
        let report = seatbelt_inside(
            dir.path().join("own").to_str().unwrap(),
            canary.to_str().unwrap(),
            dir.path().join("write").to_str().unwrap(),
            "1",
            dir.path().to_str().unwrap(),
        );
        assert!(report.get("pidIsolated").is_none());
        assert_eq!(report["foreignReadDenied"], false);
        assert!(!all_true(&report, SEATBELT_FIELDS));
    }

    #[test]
    fn unknown_probe_kinds_are_refused() {
        assert_eq!(inside(&["nope".into()]), 2);
        assert_eq!(inside(&[]), 2);
    }

    #[test]
    fn the_isolation_probe_fails_on_an_unconfined_host() {
        // Run outside any sandbox, the canary is readable: the probe must
        // report failure rather than pass vacuously.
        let dir = tempfile::tempdir().unwrap();
        let canary = dir.path().join("canary");
        std::fs::write(&canary, CANARY).unwrap();
        let report = isolation(
            dir.path().join("own").to_str().unwrap(),
            canary.to_str().unwrap(),
            dir.path().join("write").to_str().unwrap(),
            "1",
            dir.path().to_str().unwrap(),
        );
        assert_eq!(report["ownWrite"], true);
        assert_eq!(report["foreignReadDenied"], false);
        assert!(!all_true(&report, ISOLATION_FIELDS));
    }

    #[test]
    fn tunnel_reads_the_connect_reply_then_the_upstream_token() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            assert!(head.starts_with(b"CONNECT probe.invalid:443 HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut ping = [0u8; 4];
            stream.read_exact(&mut ping).unwrap();
            assert_eq!(&ping, PROBE_PING);
            stream.write_all(PROBE_TOKEN).unwrap();
        });
        let stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        assert_eq!(tunnel(stream), (true, true));
        server.join().unwrap();
    }

    #[test]
    fn a_refused_connect_is_not_a_tunnel() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 512];
            let _ = stream.read(&mut buffer).unwrap();
            stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        assert_eq!(tunnel(stream), (false, false));
        server.join().unwrap();
    }

    #[test]
    fn probe_output_parsing_takes_the_last_json_line() {
        assert_eq!(
            last_json_line(b"noise\n{\"a\":true}\n\n"),
            json!({"a": true})
        );
        assert_eq!(last_json_line(b"not json"), Value::Null);
        assert!(!all_true(&Value::Null, &["a"]));
    }

    #[tokio::test]
    async fn qualify_refuses_off_linux_or_without_bwrap() {
        if cfg!(target_os = "linux") && sandbox::bwrap_candidate().is_some() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        assert!(qualify(root.path()).await.is_err());
        assert!(!root.path().join(LINUX_QUALIFICATION_NAME).exists());
    }
}
