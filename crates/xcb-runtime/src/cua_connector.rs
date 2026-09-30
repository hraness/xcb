//! Discovery and explicit admission of the installed computer-use connector.
//!
//! Finding a launchable MCP executable is not activation evidence. The desktop
//! connector needs Codex's strict automatic-review handshake on every JS call;
//! an ordinary MCP client that omits its turn metadata skips that review. Do not
//! turn this inspection result into a generic stdio server configuration. The
//! registration below admits only the reviewed native Codex transport.

use crate::{Error, Result, digest};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const PLUGIN_DIRECTORY: &str = "plugins/cache/openai-bundled/unified-computer-use";
const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
pub const APPROVAL_BRIDGE_REQUIRED: &str = "The desktop browser and computer tools are installed, but xcb cannot yet preserve their automatic approval review. Use them in the desktop app until that connection is supported.";
const NATIVE_AVAILABLE: &str = "The desktop browser and computer tools can connect through Codex's automatic approval review. Run xcb tools setup-computer to enable the installed connector.";

/// Installation information only. No credentials or manifest environment values
/// are returned, and none of these files are executed by discovery.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledCua {
    pub manifest: PathBuf,
    pub manifest_sha256: String,
    pub enabled: bool,
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub entrypoint: PathBuf,
    pub entrypoint_sha256: String,
    pub node_repl: PathBuf,
    pub node_repl_sha256: String,
    pub enabled_tools: Vec<String>,
    pub surfaces: Vec<String>,
    pub activation_supported: bool,
    pub detail: &'static str,
}

/// Inspect the newest numerically versioned installed desktop plugin. A missing
/// install returns `None`; an invalid newest install is never silently replaced
/// with an older version. This does not launch a browser or read its profile.
pub fn inspect_installed(codex_home: &Path) -> Result<Option<InstalledCua>> {
    let directory = codex_home.join(PLUGIN_DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut candidates = Vec::new();
    for (index, entry) in entries.take(257).enumerate() {
        let entry = entry?;
        if index >= 256 {
            return Err(Error::Unavailable(
                "too many installed computer-use versions",
            ));
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(version) = version_parts(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        candidates.push((version, entry.path().join(".mcp.json")));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((_, manifest)) = candidates.pop() else {
        return Ok(None);
    };
    inspect_manifest(&manifest).map(Some)
}

fn version_parts(value: &str) -> Option<Vec<u64>> {
    if value.is_empty() || value.len() > 64 {
        return None;
    }
    let parts: Vec<_> = value.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    parts
        .into_iter()
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                None
            } else {
                part.parse().ok()
            }
        })
        .collect()
}

fn invalid() -> Error {
    Error::Unavailable("installed computer-use connector has an unsupported configuration")
}

fn absolute_path(value: Option<&str>) -> Result<PathBuf> {
    let value = value.ok_or_else(invalid)?;
    let path = PathBuf::from(value);
    if value.len() > 4096 || !path.is_absolute() || value.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(path)
}

fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid());
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(invalid());
    }
    let mut result = Vec::new();
    file.take(limit + 1).read_to_end(&mut result)?;
    if result.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(result)
}

fn inspect_manifest(path: &Path) -> Result<InstalledCua> {
    let bytes = read_regular(path, MAX_MANIFEST_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let server = value.pointer("/mcpServers/cua_repl").ok_or_else(invalid)?;
    let executable = absolute_path(server.get("command").and_then(Value::as_str))?;
    let arguments = server
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if arguments.len() != 1 {
        return Err(invalid());
    }
    let entrypoint = absolute_path(arguments[0].as_str())?;
    if !entrypoint.ends_with("@oai/cua-repl/bin/cua-repl.mjs") {
        return Err(invalid());
    }
    let node_repl = absolute_path(
        server
            .pointer("/env/CUA_REPL_NODE_REPL_PATH")
            .and_then(Value::as_str),
    )?;
    let enabled_tools = server
        .get("enabled_tools")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?
        .iter()
        .map(|tool| {
            let name = tool.as_str().ok_or_else(invalid)?;
            if !["js", "js_reset", "turn_ended"].contains(&name) {
                return Err(invalid());
            }
            Ok(name.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    if !enabled_tools.iter().any(|name| name == "js") || enabled_tools.len() > 3 {
        return Err(invalid());
    }
    let surfaces = server
        .pointer("/env/CUA_REPL_ENABLED_SURFACES")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?
        .split(',')
        .map(str::trim)
        .map(|surface| {
            if !["browser", "computer"].contains(&surface) {
                return Err(invalid());
            }
            Ok(surface.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    if surfaces.is_empty() || surfaces.len() > 2 {
        return Err(invalid());
    }
    Ok(InstalledCua {
        manifest: path.to_owned(),
        manifest_sha256: digest(bytes),
        enabled: server
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        executable_sha256: crate::process::executable_digest(&executable)?,
        executable,
        entrypoint_sha256: digest(read_regular(&entrypoint, 1024 * 1024)?),
        entrypoint,
        node_repl_sha256: crate::process::executable_digest(&node_repl)?,
        node_repl,
        enabled_tools,
        surfaces,
        activation_supported: cfg!(target_os = "macos"),
        detail: if cfg!(target_os = "macos") {
            NATIVE_AVAILABLE
        } else {
            APPROVAL_BRIDGE_REQUIRED
        },
    })
}

/// Snapshot the known installed connector and its complete code dependencies.
/// Account authority stays in the existing desktop Codex home; provider
/// processes see only the private native relay, never that home or environment.
#[cfg(target_os = "macos")]
pub fn registration(
    codex_home: &Path,
    bundle_store: &Path,
    consumer_workspace: &Path,
) -> Result<crate::capabilities::CapabilityServer> {
    use crate::{
        capabilities::{CapabilityFeature, CapabilityServer, CapabilityTransport},
        capability_bundle::{self, BundleLimits},
    };
    let installed = inspect_installed(codex_home)?.ok_or(Error::Unavailable(
        "install the desktop computer-use plugin first",
    ))?;
    if !installed.enabled
        || !installed
            .enabled_tools
            .iter()
            .any(|name| name == "turn_ended")
    {
        return Err(Error::Unavailable(
            "the installed computer-use connector or its cleanup tool is disabled",
        ));
    }
    let runtime = installed
        .executable
        .parent()
        .and_then(Path::parent)
        .ok_or_else(invalid)?;
    let resources = runtime.parent().ok_or_else(invalid)?;
    let app = resources
        .parent()
        .and_then(Path::parent)
        .ok_or_else(invalid)?;
    if runtime.file_name().and_then(|name| name.to_str()) != Some("cua_node")
        || installed.executable != runtime.join("bin/node")
        || installed.entrypoint != runtime.join("lib/node_modules/@oai/cua-repl/bin/cua-repl.mjs")
        || installed.node_repl != runtime.join("bin/node_repl")
        || resources.file_name().and_then(|name| name.to_str()) != Some("Resources")
    {
        return Err(invalid());
    }
    let sky = codex_home.join("computer-use/Codex Computer Use.app");
    let codex = resources.join("codex");
    let manifest_bytes = read_regular(&installed.manifest, MAX_MANIFEST_BYTES)?;
    if digest(&manifest_bytes) != installed.manifest_sha256 {
        return Err(invalid());
    }
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let environment = admitted_environment(
        &manifest["mcpServers"]["cua_repl"]["env"],
        codex_home,
        runtime,
        &sky,
        &codex,
    )?;
    verify_vendor_bundle(app, "com.openai.codex")?;
    verify_vendor_bundle(&sky, "com.openai.sky.CUAService")?;
    let limits = BundleLimits::default();
    let runtime_bundle =
        capability_bundle::snapshot(runtime, bundle_store, consumer_workspace, limits)?;
    let sky_bundle = capability_bundle::snapshot(&sky, bundle_store, consumer_workspace, limits)?;
    let codex_bundle =
        capability_bundle::snapshot_file(&codex, bundle_store, consumer_workspace, limits)?;
    verify_vendor_bundle(&sky_bundle.root, "com.openai.sky.CUAService")?;
    let new_codex = codex_bundle.root.join("codex");
    let mut environment = environment;
    for (key, relative) in [
        ("NODE_REPL_NODE_MODULE_DIRS", "lib/node_modules"),
        ("NODE_REPL_NODE_PATH", "bin/node"),
        ("NODE_REPL_TRUSTED_CODE_PATHS", "lib/node_modules"),
        ("CUA_REPL_NODE_REPL_PATH", "bin/node_repl"),
    ] {
        environment.insert(
            key.into(),
            runtime_bundle
                .root
                .join(relative)
                .to_string_lossy()
                .into_owned(),
        );
    }
    environment.insert(
        "SKY_CUA_SERVICE_PATH".into(),
        sky_bundle.root.to_string_lossy().into_owned(),
    );
    environment.insert(
        "CODEX_CLI_PATH".into(),
        new_codex.to_string_lossy().into_owned(),
    );
    let executable = runtime_bundle.root.join("bin/node");
    let server = CapabilityServer {
        credential_account: None,
        name: "cua_repl".into(),
        transport: CapabilityTransport::CodexNative,
        executable: executable.clone(),
        sha256: crate::process::executable_digest(&executable)?,
        args: vec![
            runtime_bundle
                .root
                .join("lib/node_modules/@oai/cua-repl/bin/cua-repl.mjs")
                .to_string_lossy()
                .into_owned(),
        ],
        bundles: vec![runtime_bundle, sky_bundle, codex_bundle],
        env: vec![],
        environment,
        tools: Some(installed.enabled_tools),
        shutdown_tool: Some("turn_ended".into()),
        features: installed
            .surfaces
            .iter()
            .map(|surface| {
                if surface == "browser" {
                    CapabilityFeature::Browser
                } else {
                    CapabilityFeature::Computer
                }
            })
            .collect(),
        timeout_ms: 120_000,
    };
    server.validate()?;
    Ok(server)
}

#[cfg(not(target_os = "macos"))]
pub fn registration(
    _codex_home: &Path,
    _bundle_store: &Path,
    _consumer_workspace: &Path,
) -> Result<crate::capabilities::CapabilityServer> {
    Err(Error::Unavailable(
        "the installed desktop computer-use connector requires macOS",
    ))
}

#[cfg(target_os = "macos")]
fn verify_vendor_bundle(path: &Path, identifier: &str) -> Result<()> {
    let requirement = format!(
        "=anchor apple generic and identifier \"{identifier}\" and certificate leaf[subject.OU] = \"2DC432GLL2\""
    );
    let status = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "-R", &requirement])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Unavailable(
            "installed computer-use code failed OpenAI signature verification",
        ))
    }
}

#[cfg(any(target_os = "macos", test))]
fn admitted_environment(
    value: &Value,
    codex_home: &Path,
    runtime: &Path,
    sky: &Path,
    codex: &Path,
) -> Result<std::collections::BTreeMap<String, String>> {
    const KEYS: &[&str] = &[
        "NODE_REPL_NATIVE_PIPE_CONNECT_TIMEOUT_MS",
        "NODE_REPL_NODE_MODULE_DIRS",
        "NODE_REPL_NODE_PATH",
        "NODE_REPL_TRUSTED_CODE_PATHS",
        "CODEX_HOME",
        "BROWSER_USE_AVAILABLE_BACKENDS",
        "BROWSER_USE_TINYSKY_ENABLED",
        "NODE_REPL_INSTRUCTIONS_USE_CASE_BROWSER",
        "NODE_REPL_INSTRUCTIONS_USE_CASE_CHROME",
        "NODE_REPL_INSTRUCTIONS_USE_CASE_COMPUTER_USE",
        "BROWSER_USE_CODEX_APP_BUILD_FLAVOR",
        "BROWSER_USE_CODEX_APP_VERSION",
        "NODE_REPL_TRUSTED_SERVICES",
        "SKY_CUA_SERVICE_PATH",
        "CODEX_CLI_PATH",
        "CUA_REPL_NODE_REPL_PATH",
        "CUA_REPL_ENABLED_SURFACES",
    ];
    let env = value.as_object().ok_or_else(invalid)?;
    if env.len() != KEYS.len() || env.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return Err(invalid());
    }
    let mut admitted = std::collections::BTreeMap::new();
    for (key, value) in env {
        let value = value
            .as_str()
            .filter(|value| value.len() <= 4096 && !value.contains('\0'))
            .ok_or_else(invalid)?;
        admitted.insert(key.clone(), value.to_owned());
    }
    for (key, path) in [
        ("CODEX_HOME", codex_home.to_owned()),
        (
            "NODE_REPL_NODE_MODULE_DIRS",
            runtime.join("lib/node_modules"),
        ),
        ("NODE_REPL_NODE_PATH", runtime.join("bin/node")),
        ("CUA_REPL_NODE_REPL_PATH", runtime.join("bin/node_repl")),
        ("SKY_CUA_SERVICE_PATH", sky.to_owned()),
        ("CODEX_CLI_PATH", codex.to_owned()),
    ] {
        if Path::new(&admitted[key]) != path {
            return Err(invalid());
        }
    }
    let services: Value = serde_json::from_str(&admitted["NODE_REPL_TRUSTED_SERVICES"])?;
    if services
        != serde_json::json!({"browser":"@oai/browser-desktop/service","sky":"@oai/sky/service"})
        || admitted["NODE_REPL_TRUSTED_CODE_PATHS"]
            != format!(
                "{}:{}",
                codex_home.display(),
                runtime.join("lib/node_modules").display()
            )
        || !admitted["NODE_REPL_NATIVE_PIPE_CONNECT_TIMEOUT_MS"]
            .parse::<u64>()
            .ok()
            .is_some_and(|timeout| (100..=30_000).contains(&timeout))
        || !["0", "1"].contains(&admitted["BROWSER_USE_TINYSKY_ENABLED"].as_str())
        || admitted["BROWSER_USE_AVAILABLE_BACKENDS"]
            .split(',')
            .any(|backend| !["chrome", "edge", "iab"].contains(&backend))
    {
        return Err(invalid());
    }
    Ok(admitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_environment_is_closed_and_paths_are_exact() {
        let home = Path::new("/owner/.codex");
        let runtime = Path::new("/app/Contents/Resources/cua_node");
        let sky = home.join("computer-use/Codex Computer Use.app");
        let codex = Path::new("/app/Contents/Resources/codex");
        let env = serde_json::json!({
            "NODE_REPL_NATIVE_PIPE_CONNECT_TIMEOUT_MS":"1000",
            "NODE_REPL_NODE_MODULE_DIRS":runtime.join("lib/node_modules"),
            "NODE_REPL_NODE_PATH":runtime.join("bin/node"),
            "NODE_REPL_TRUSTED_CODE_PATHS":format!("{}:{}",home.display(),runtime.join("lib/node_modules").display()),
            "CODEX_HOME":home,"BROWSER_USE_AVAILABLE_BACKENDS":"chrome,iab","BROWSER_USE_TINYSKY_ENABLED":"1",
            "NODE_REPL_INSTRUCTIONS_USE_CASE_BROWSER":"browser instructions", "NODE_REPL_INSTRUCTIONS_USE_CASE_CHROME":"chrome instructions", "NODE_REPL_INSTRUCTIONS_USE_CASE_COMPUTER_USE":"computer instructions",
            "BROWSER_USE_CODEX_APP_BUILD_FLAVOR":"prod", "BROWSER_USE_CODEX_APP_VERSION":"26.915.31945",
            "NODE_REPL_TRUSTED_SERVICES":r#"{"browser":"@oai/browser-desktop/service","sky":"@oai/sky/service"}"#,
            "SKY_CUA_SERVICE_PATH":sky,"CODEX_CLI_PATH":codex,"CUA_REPL_NODE_REPL_PATH":runtime.join("bin/node_repl"),"CUA_REPL_ENABLED_SURFACES":"browser,computer"
        });
        assert!(admitted_environment(&env, home, runtime, &sky, codex).is_ok());
        let mut bad = env.clone();
        bad["PRIVATE_TOKEN"] = Value::String("must never be copied".into());
        assert!(admitted_environment(&bad, home, runtime, &sky, codex).is_err());
        let mut bad = env.clone();
        bad["CODEX_CLI_PATH"] = Value::String("/workspace/agent-controlled-program".into());
        assert!(admitted_environment(&bad, home, runtime, &sky, codex).is_err());
        let mut bad = env.clone();
        bad["NODE_REPL_TRUSTED_SERVICES"] =
            Value::String(r#"{"browser":"/workspace/server"}"#.into());
        assert!(admitted_environment(&bad, home, runtime, &sky, codex).is_err());
        let mut bad = env;
        bad["NODE_REPL_TRUSTED_CODE_PATHS"] = Value::String("/workspace".into());
        assert!(admitted_environment(&bad, home, runtime, &sky, codex).is_err());
    }

    #[test]
    fn versions_are_numeric_and_bounded() {
        assert!(version_parts("26.10.1") > version_parts("26.9.100"));
        for invalid in [
            "",
            "latest",
            "26",
            "26.0-beta",
            "26..1",
            "26.1.2.3.4",
            "26.+3",
        ] {
            assert!(version_parts(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn missing_plugin_does_not_modify_profile() {
        let root = tempfile::tempdir().unwrap();
        assert!(inspect_installed(root.path()).unwrap().is_none());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn newest_invalid_install_is_not_replaced_with_older_install() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(PLUGIN_DIRECTORY);
        for version in ["26.9.100", "26.10.1"] {
            fs::create_dir_all(directory.join(version)).unwrap();
        }
        fs::write(directory.join("26.9.100/.mcp.json"), "{}").unwrap();
        let error = inspect_installed(root.path()).unwrap_err();
        assert!(
            matches!(error, Error::Io(ref error) if error.kind() == std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn unbounded_manifest_is_rejected_before_parsing() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join(".mcp.json");
        fs::write(&file, vec![b' '; (MAX_MANIFEST_BYTES + 1) as usize]).unwrap();
        assert!(matches!(
            inspect_manifest(&file),
            Err(Error::Unavailable(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inspection_hashes_launch_artifacts_but_never_exposes_environment_or_activates() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join(PLUGIN_DIRECTORY).join("26.10.1");
        fs::create_dir_all(&plugin).unwrap();
        let executable = root.path().join("node");
        let node_repl = root.path().join("node_repl");
        for path in [&executable, &node_repl] {
            fs::write(path, "#!/bin/sh\nexit 9\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let entrypoint = root.path().join("@oai/cua-repl/bin/cua-repl.mjs");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(&entrypoint, "throw new Error('must not execute');\n").unwrap();
        let manifest = serde_json::json!({"mcpServers":{"cua_repl":{
            "command":executable,"args":[entrypoint],"enabled":true,
            "enabled_tools":["js","js_reset","turn_ended"],
            "env":{"CUA_REPL_NODE_REPL_PATH":node_repl,"CUA_REPL_ENABLED_SURFACES":"browser,computer",
                "PRIVATE_TOKEN":"secret-fixture-never-exposed"}
        }}});
        fs::write(
            plugin.join(".mcp.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let report = inspect_installed(root.path()).unwrap().unwrap();
        assert_eq!(report.activation_supported, cfg!(target_os = "macos"));
        assert_eq!(report.executable_sha256, digest("#!/bin/sh\nexit 9\n"));
        assert_eq!(report.node_repl_sha256, report.executable_sha256);
        assert_eq!(report.surfaces, ["browser", "computer"]);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("PRIVATE_TOKEN"));
        assert!(!serialized.contains("secret-fixture"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_is_not_read() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("real.json");
        fs::write(&file, "{}").unwrap();
        let link = root.path().join(".mcp.json");
        std::os::unix::fs::symlink(file, &link).unwrap();
        assert!(matches!(
            inspect_manifest(&link),
            Err(Error::Unavailable(_))
        ));
    }
}
