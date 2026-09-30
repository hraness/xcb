//! Read-only host telemetry and an explicit switch for managed launch limits.
use clap::Subcommand;
use std::path::{Path, PathBuf};
use xcb_runtime::{Error, Result, config::Config, host_resources, now_ms, private};

#[derive(Subcommand)]
pub enum ResourceCommand {
    /// Defer new managed work when memory or disk pressure is unsafe.
    Enable,
    /// Stop deferring managed work for host pressure; keep telemetry available.
    Disable,
}

pub async fn dispatch(
    root: &Path,
    workspace: Option<PathBuf>,
    command: Option<ResourceCommand>,
    json: bool,
) -> Result<i32> {
    let root = if command.is_some() {
        private::directory(root)?
    } else {
        xcb_core::canonical(root)?
    };
    let (mut config, revision) = Config::load(&root)?;
    if matches!(command, Some(ResourceCommand::Disable)) {
        config.resources.enabled = false;
        config.save(&root, revision.as_deref())?;
        if json {
            println!(
                "{}",
                serde_json::json!({ "version": 1, "enabled": false, "policy": config.resources })
            );
        } else {
            println!("Host resource protection: off. Existing tasks continue normally.");
        }
        return Ok(0);
    }
    if matches!(command, Some(ResourceCommand::Enable)) && !host_resources::supported_platform() {
        return Err(Error::Unavailable(
            "host resource protection currently supports macOS and Linux",
        ));
    }
    let workspace = workspace.map(xcb_core::canonical).transpose()?;
    let paths: Vec<_> = workspace.iter().cloned().collect();
    let snapshot = host_resources::collect(&root, &paths).await;
    snapshot.validate().map_err(|message| Error::Guided {
        message,
        next: None,
    })?;
    // Enabling a guard that cannot observe this host would strand all new
    // work. Require usable measurements first; no provider is launched.
    if matches!(command, Some(ResourceCommand::Enable))
        && (snapshot.memory.pressure == host_resources::MemoryPressure::Unknown
            || snapshot.disks.iter().any(|disk| disk.free_bytes.is_none()))
    {
        return Err(Error::Unavailable(
            "host telemetry is unavailable; fix xcb resources before enabling protection",
        ));
    }
    if let Some(command) = command {
        config.resources.enabled = matches!(command, ResourceCommand::Enable);
        config.save(&root, revision.as_deref())?;
    }
    let mut monitor = host_resources::Monitor::new();
    monitor.observe(snapshot.clone());
    let assessment = monitor.assess(
        &config.resources,
        workspace.as_deref().unwrap_or(&root),
        now_ms(),
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1, "enabled": config.resources.enabled,
                "snapshot": snapshot, "assessment": assessment,
                "policy": config.resources,
                "scope": "current_sample",
            }))?
        );
    } else {
        println!(
            "Host resource protection: {}",
            if config.resources.enabled {
                "enabled"
            } else {
                "off (observe only)"
            }
        );
        println!("Memory pressure: {:?}", snapshot.memory.pressure);
        if let Some(bytes) = snapshot.memory.swap_used_bytes {
            println!(
                "Swap used: {:.2} GiB",
                bytes as f64 / (1024_u64.pow(3) as f64)
            );
        }
        for disk in &snapshot.disks {
            match disk.free_bytes {
                Some(bytes) => println!(
                    "{}: {:.1} GiB available",
                    disk.path.display(),
                    bytes as f64 / (1024_u64.pow(3) as f64)
                ),
                None => println!("{}: disk measurement unavailable", disk.path.display()),
            }
        }
        for reason in assessment.reasons.iter().chain(&assessment.advisories) {
            println!("{reason}");
        }
        println!(
            "This is one current sample; the supervisor evaluates sustained pressure and recovery over time."
        );
    }
    Ok(0)
}
