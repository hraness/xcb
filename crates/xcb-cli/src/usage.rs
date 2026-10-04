//! `xcb usage`: token use across your coding agents, from aicharts.
//!
//! aicharts keeps a daily record on this computer (`aicharts history`). xcb
//! runs a fixed set of its report and scheduling commands and never asks it to
//! enroll or publish, so nothing here sends usage anywhere. xcb's own quota
//! measurement for routing (`xcb accounts`) is separate and unchanged.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where people get aicharts when it is missing.
pub const GET_AICHARTS: &str = "https://aicharts.io/usage";
/// The aicharts history commands xcb forwards. Publishing, enrollment and
/// every other aicharts command stay out of reach from xcb.
const COMMANDS: [&str; 5] = ["report", "status", "enable", "disable", "collect"];

pub const HELP: &str = "\
Your token use across coding agents, by day, agent, provider and model. The
numbers come from aicharts, which keeps a daily record on this computer and
uploads nothing.

Usage: xcb usage [report] [--days N | --since YYYY-MM-DD --until YYYY-MM-DD]
                 [--client ID ...] [--csv]
       xcb usage status | enable | disable | collect | connect | disconnect

Commands
  report     Token use from the record (default: the last 30 days)
  status     Whether aicharts collects, what the record holds, when it ran
  enable     Collect four times a day on this computer
  disable    Stop collecting; the record stays
  collect    Read your agents' session files into the record now
  connect    Let Claude and Codex tasks read the record through aicharts'
             read-only tools (xcb pins this aicharts build; run it again
             after updating aicharts)
  disconnect Remove those tools from tasks

With --json, report prints the aicharts report format, which you can open at
aicharts.io/usage/details without uploading it. Agents can read the same
record through `aicharts mcp`. Quota left on each account: xcb accounts.

Examples
  xcb usage
  xcb usage report --days 7 --client claude
  xcb usage report --since 2026-09-01 --until 2026-09-30 --csv > september.csv
";

/// The tool server name and the read-only tools `xcb usage connect` exposes.
pub const SERVER: &str = "aicharts";
const MCP_TOOLS: [&str; 5] = [
    "usage_summary",
    "usage_daily",
    "usage_report",
    "usage_clients",
    "usage_history_status",
];

/// `connect` and `disconnect` change xcb's tool registrations, so unlike the
/// report commands they run with xcb's state.
pub fn needs_state(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("connect" | "disconnect")
    )
}

/// The host tool registration for aicharts' read-only MCP server, pinned to
/// this executable's digest like any `xcb tools add` definition.
pub fn registration(
    executable: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> xcb_runtime::Result<xcb_runtime::capabilities::CapabilityServer> {
    let executable = std::fs::canonicalize(executable)?;
    let sha256 = xcb_runtime::process::executable_digest(&executable)?;
    // xcb gives each tool server a private home, so name aicharts' own folder,
    // where the record lives, explicitly.
    let home = env("AICHARTS_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".aicharts")))
        .filter(|path| path.is_absolute())
        .ok_or(xcb_runtime::Error::Unavailable(
            "HOME or AICHARTS_HOME must name an absolute folder for aicharts' record",
        ))?;
    let server: xcb_runtime::capabilities::CapabilityServer =
        serde_json::from_value(serde_json::json!({
            "name": SERVER,
            "executable": executable,
            "sha256": sha256,
            "args": ["mcp"],
            "environment": {"AICHARTS_HOME": home, "HRANESS_SUPPORT_AUDIENCE": "off"},
            "tools": MCP_TOOLS,
        }))?;
    server.validate()?;
    Ok(server)
}

/// Adds or removes the aicharts tool server in xcb's configuration.
pub fn connect(
    store: &xcb_runtime::store::Store,
    args: &[String],
    json: bool,
) -> xcb_runtime::Result<i32> {
    let connecting = matches!(args, [command] if command == "connect");
    if !connecting && !matches!(args, [command] if command == "disconnect") {
        eprintln!("xcb usage connect and xcb usage disconnect take no arguments.");
        return Ok(2);
    }
    let (mut config, revision) = xcb_runtime::config::Config::load(store.root())?;
    let server = if connecting {
        let Some(program) = locate(|name| std::env::var_os(name)) else {
            return Ok(missing(json));
        };
        Some(registration(&program, |name| std::env::var_os(name))?)
    } else {
        None
    };
    let had = config
        .capabilities
        .servers
        .iter()
        .any(|entry| entry.name == SERVER);
    config
        .capabilities
        .servers
        .retain(|entry| entry.name != SERVER);
    if let Some(server) = &server {
        config.capabilities.servers.push(server.clone());
    }
    config.save(store.root(), revision.as_deref())?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "connected": server.is_some(),
                "executable": server.as_ref().map(|server| &server.executable),
                "sha256": server.as_ref().map(|server| &server.sha256),
            })
        );
    } else if let Some(server) = &server {
        println!(
            "Claude and Codex tasks can now read your usage record through aicharts' read-only tools ({}).",
            server.executable.display()
        );
        println!(
            "xcb checks that build before each launch; run xcb usage connect again after updating aicharts."
        );
    } else if had {
        println!("Removed aicharts' usage tools from tasks.");
    } else {
        println!("aicharts' usage tools were not connected.");
    }
    Ok(0)
}

/// What `xcb doctor` reports about local usage history.
pub async fn doctor_status(config: &xcb_runtime::config::Config) -> serde_json::Value {
    let registered = config
        .capabilities
        .servers
        .iter()
        .find(|entry| entry.name == SERVER);
    let Some(program) = locate(|name| std::env::var_os(name)) else {
        return serde_json::json!({"aicharts": null, "connected": registered.is_some()});
    };
    let output = |args: &'static [&'static str]| {
        let program = program.clone();
        async move {
            let child = tokio::process::Command::new(&program)
                .args(args)
                .env("HRANESS_SUPPORT_AUDIENCE", "off")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .output();
            match tokio::time::timeout(std::time::Duration::from_secs(5), child).await {
                Ok(Ok(out)) if out.status.success() => {
                    Some(String::from_utf8_lossy(&out.stdout).into_owned())
                }
                _ => None,
            }
        }
    };
    let version = output(&["--version"])
        .await
        .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned));
    let collecting = output(&["history", "status", "--json"])
        .await
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value["data"]["collecting"].as_str().map(str::to_owned));
    let current = std::fs::canonicalize(&program)
        .ok()
        .and_then(|path| xcb_runtime::process::executable_digest(&path).ok());
    let connected = match registered {
        None => "no",
        Some(entry) if Some(&entry.sha256) == current.as_ref() => "yes",
        Some(_) => "stale",
    };
    serde_json::json!({
        "aicharts": program,
        "version": version,
        "collecting": collecting,
        "connected": connected,
    })
}

/// The aicharts arguments for `xcb usage ARGS`, or a sentence explaining why
/// the request is refused.
pub fn plan(args: &[String], json: bool) -> Result<Vec<String>, String> {
    let (command, rest) = match args.split_first() {
        None => ("report", &[][..]),
        Some((first, rest)) if COMMANDS.contains(&first.as_str()) => (first.as_str(), rest),
        Some((first, _)) if first.starts_with('-') => ("report", args),
        Some((first, _)) => {
            return Err(format!(
                "xcb usage has no \"{first}\" command. Use report, status, enable, disable or collect."
            ));
        }
    };
    let mut argv = vec!["history".to_owned(), command.to_owned()];
    argv.extend(rest.iter().cloned());
    if json && !argv.iter().any(|arg| arg == "--json") {
        argv.push("--json".to_owned());
    }
    Ok(argv)
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// `XCB_AICHARTS` when it names an absolute executable, else the first
/// `aicharts` on PATH, else `~/.local/bin/aicharts`, where the xcb and
/// aicharts installers put it.
pub fn locate(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "aicharts.exe"
    } else {
        "aicharts"
    };
    if let Some(explicit) = env("XCB_AICHARTS").map(PathBuf::from) {
        return (explicit.is_absolute() && executable(&explicit)).then_some(explicit);
    }
    if let Some(path) = env("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if directory.is_absolute() && executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    let local = PathBuf::from(env("HOME")?)
        .join(".local")
        .join("bin")
        .join(name);
    (local.is_absolute() && executable(&local)).then_some(local)
}

fn missing(json: bool) -> i32 {
    let message = "Usage reports come from aicharts, which isn't installed on this computer.";
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": false,
                "error": {"code": "aicharts_missing", "message": message, "next": GET_AICHARTS},
            })
        );
    } else {
        eprintln!("{message}\nGet it at {GET_AICHARTS}, then run xcb usage again.");
    }
    1
}

/// Runs `aicharts history ...` with xcb's terminal and returns its exit code.
pub fn run(args: &[String], json: bool) -> i32 {
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        print!("{HELP}");
        return 0;
    }
    let argv = match plan(args, json) {
        Ok(argv) => argv,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    let Some(program) = locate(|name| std::env::var_os(name)) else {
        return missing(json);
    };
    // A delegated aicharts call stays quiet about its own updates and support
    // offers; xcb owns this conversation.
    match Command::new(&program)
        .args(&argv)
        .env("HRANESS_SUPPORT_AUDIENCE", "off")
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => missing(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn plain_usage_reports_and_options_go_to_the_report() {
        assert_eq!(plan(&[], false).unwrap(), args(&["history", "report"]));
        assert_eq!(
            plan(&args(&["--days", "7"]), false).unwrap(),
            args(&["history", "report", "--days", "7"])
        );
        assert_eq!(
            plan(&args(&["status"]), true).unwrap(),
            args(&["history", "status", "--json"])
        );
        assert_eq!(
            plan(&args(&["report", "--json"]), true).unwrap(),
            args(&["history", "report", "--json"]),
            "never doubled"
        );
    }

    #[test]
    fn publishing_and_other_aicharts_commands_are_out_of_reach() {
        for refused in [
            "publish",
            "enroll",
            "setup",
            "sync",
            "upload",
            "autosubmit",
            "mcp",
        ] {
            assert!(plan(&args(&[refused]), false).is_err(), "{refused}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_connect_and_disconnect_need_xcb_state() {
        assert!(needs_state(&args(&["connect"])));
        assert!(needs_state(&args(&["disconnect"])));
        for report in [
            &[][..],
            &["status"][..],
            &["report", "--days", "7"][..],
            &["--csv"][..],
        ] {
            assert!(!needs_state(&args(report)));
        }
    }

    #[test]
    fn the_registration_pins_the_build_and_names_the_record() {
        let folder =
            std::env::temp_dir().join(format!("xcb-usage-registration-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let program = folder.join("aicharts");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = folder.join("home");
        let env = |name: &str| (name == "HOME").then(|| home.clone().into_os_string());
        let server = registration(&program, env).unwrap();
        assert_eq!(server.name, SERVER);
        assert_eq!(server.args, ["mcp"]);
        assert_eq!(server.executable, std::fs::canonicalize(&program).unwrap());
        assert_eq!(
            server.sha256,
            xcb_runtime::process::executable_digest(&server.executable).unwrap()
        );
        assert_eq!(
            server.environment["AICHARTS_HOME"],
            home.join(".aicharts").to_string_lossy()
        );
        assert_eq!(server.environment["HRANESS_SUPPORT_AUDIENCE"], "off");
        assert_eq!(server.tools.as_deref().unwrap(), MCP_TOOLS);
        assert!(server.env.is_empty());
        let explicit = folder.join("record");
        let env = |name: &str| match name {
            "AICHARTS_HOME" => Some(explicit.clone().into_os_string()),
            "HOME" => Some(home.clone().into_os_string()),
            _ => None,
        };
        assert_eq!(
            registration(&program, env).unwrap().environment["AICHARTS_HOME"],
            explicit.to_string_lossy()
        );
        assert!(registration(&program, |_| None).is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn locate_prefers_the_explicit_path_then_path_then_local_bin() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("xcb-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let local = root.join("home/.local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for path in [bin.join("aicharts"), local.join("aicharts")] {
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = root.join("home");
        let env = |path: Option<&Path>, explicit: Option<&Path>| {
            let path = path.map(|path| path.as_os_str().to_owned());
            let explicit = explicit.map(|explicit| explicit.as_os_str().to_owned());
            let home = home.as_os_str().to_owned();
            move |name: &str| match name {
                "PATH" => path.clone(),
                "XCB_AICHARTS" => explicit.clone(),
                "HOME" => Some(home.clone()),
                _ => None,
            }
        };
        assert_eq!(locate(env(Some(&bin), None)), Some(bin.join("aicharts")));
        assert_eq!(locate(env(None, None)), Some(local.join("aicharts")));
        assert_eq!(
            locate(env(Some(&bin), Some(&local.join("aicharts")))),
            Some(local.join("aicharts"))
        );
        assert_eq!(
            locate(env(Some(&bin), Some(Path::new("relative/aicharts")))),
            None
        );
        std::fs::set_permissions(bin.join("aicharts"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert_eq!(
            locate(env(Some(&bin), None)),
            Some(local.join("aicharts")),
            "not executable"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
