use super::*;

fn fixture_path(path: &str) -> PathBuf {
    // Rooted paths need the current drive to be absolute on Windows.
    std::path::absolute(path).expect("absolute synthetic fixture path")
}

fn policy() -> ResourcePolicy {
    ResourcePolicy {
        enabled: true,
        ..ResourcePolicy::default()
    }
}

fn disk(path: &str, free: u64) -> DiskSnapshot {
    DiskSnapshot {
        path: fixture_path(path),
        volume_id: Some(1),
        free_bytes: Some(free),
        total_bytes: Some(1024 * GIB),
        error: None,
    }
}

fn sample(at_ms: u64, pressure: MemoryPressure, free: u64) -> Snapshot {
    Snapshot {
        schema_version: 1,
        at_ms,
        state_root: fixture_path("/state"),
        memory: MemorySnapshot {
            pressure,
            physical_total_bytes: Some(128 * GIB),
            swap_used_bytes: Some(0),
        },
        disks: vec![disk("/state", 100 * GIB), disk("/work", free)],
        errors: Vec::new(),
    }
}

fn assess(monitor: &mut Monitor, at_ms: u64, pressure: MemoryPressure, free: u64) -> Assessment {
    monitor.observe(sample(at_ms, pressure, free));
    monitor.assess(&policy(), &fixture_path("/work"), at_ms)
}

#[test]
fn disabled_policy_does_not_change_existing_admission() {
    let mut monitor = Monitor::new();
    assert!(
        !monitor
            .assess(&ResourcePolicy::default(), &fixture_path("/work"), 1)
            .blocked
    );
    assert!(monitor.assess(&policy(), &fixture_path("/work"), 1).blocked);
}

#[test]
fn policy_requires_ordered_thresholds_and_bounded_cadence() {
    assert!(policy().validate().is_ok());
    let mut invalid = policy();
    invalid.pause_disk_bytes = invalid.resume_disk_bytes;
    assert!(invalid.validate().is_err());
    invalid = policy();
    invalid.sample_interval_secs = 1;
    assert!(invalid.validate().is_err());
    invalid = policy();
    invalid.stale_after_secs = 30;
    assert!(invalid.validate().is_err());
    invalid = policy();
    invalid.recovery_samples = 1;
    assert!(invalid.validate().is_err());
    assert!(serde_json::from_str::<ResourcePolicy>(r#"{"enabled":true,"typo":42}"#).is_err());
}

#[test]
fn disk_pauses_immediately_at_boundary_and_recovers_after_three_distinct_samples() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, 24 * GIB).blocked);
    assert!(assess(&mut monitor, 31_000, MemoryPressure::Normal, 40 * GIB).blocked);
    for _ in 0..30 {
        assert!(
            monitor
                .assess(&policy(), &fixture_path("/work"), 31_000)
                .blocked
        );
        monitor.observe(sample(31_000, MemoryPressure::Normal, 40 * GIB));
    }
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert!(!assess(&mut monitor, 91_000, MemoryPressure::Normal, 40 * GIB).blocked);
}

#[test]
fn disk_must_recover_above_resume_not_at_boundary() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, GIB).blocked);
    for at in [31_000, 61_000, 91_000] {
        assert!(assess(&mut monitor, at, MemoryPressure::Normal, 32 * GIB).blocked);
    }
    assert!(assess(&mut monitor, 121_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert!(assess(&mut monitor, 151_000, MemoryPressure::Normal, 30 * GIB).blocked);
    assert!(assess(&mut monitor, 181_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert!(assess(&mut monitor, 211_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert!(!assess(&mut monitor, 241_000, MemoryPressure::Normal, 40 * GIB).blocked);
}

#[test]
fn both_state_and_selected_workspace_reserves_are_required() {
    let mut monitor = Monitor::new();
    let mut observation = sample(1_000, MemoryPressure::Normal, 100 * GIB);
    observation.disks[0].free_bytes = Some(GIB);
    observation.disks.push(disk("/other", 100 * GIB));
    monitor.observe(observation);
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 1_000)
            .blocked
    );
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/other"), 1_000)
            .blocked
    );
}

#[test]
fn unrelated_low_workspace_does_not_block_healthy_workspace() {
    let mut monitor = Monitor::new();
    let mut observation = sample(1_000, MemoryPressure::Normal, GIB);
    observation.disks.push(disk("/other", 100 * GIB));
    monitor.observe(observation);
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 1_000)
            .blocked
    );
    assert!(
        !monitor
            .assess(&policy(), &fixture_path("/other"), 1_000)
            .blocked
    );
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/missing"), 1_000)
            .blocked
    );
}

#[test]
fn warning_is_advisory_and_sustained_pressure_stops_admission() {
    let mut monitor = Monitor::new();
    let first = assess(&mut monitor, 1_000, MemoryPressure::Warning, 100 * GIB);
    assert!(!first.blocked);
    assert!(!first.advisories.is_empty());
    assert!(!assess(&mut monitor, 31_000, MemoryPressure::Critical, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Critical, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 91_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 121_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(!assess(&mut monitor, 151_000, MemoryPressure::Normal, 100 * GIB).blocked);
}

#[test]
fn failed_sampler_stales_and_never_counts_same_observation_as_sustained_pressure() {
    let mut monitor = Monitor::new();
    assert!(!assess(&mut monitor, 1_000, MemoryPressure::Critical, 100 * GIB).blocked);
    assert!(
        !monitor
            .assess(&policy(), &fixture_path("/work"), 91_000)
            .blocked
    );
    let stale = monitor.assess(&policy(), &fixture_path("/work"), 122_000);
    assert!(stale.blocked);
    assert!(stale.reasons[0].contains("stale"));
    assert!(!assess(&mut monitor, 151_000, MemoryPressure::Critical, 100 * GIB).blocked);
}

#[test]
fn stale_gap_resets_recovery_samples() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, GIB).blocked);
    assert!(assess(&mut monitor, 31_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 182_000)
            .blocked
    );
    assert!(assess(&mut monitor, 211_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 241_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(!assess(&mut monitor, 271_000, MemoryPressure::Normal, 100 * GIB).blocked);
}

#[test]
fn unknown_memory_failed_disk_future_and_invalid_observations_fail_closed() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Unknown, 100 * GIB).blocked);
    let mut failed = sample(31_000, MemoryPressure::Normal, 100 * GIB);
    failed.disks[1].free_bytes = None;
    failed.disks[1].total_bytes = None;
    failed.disks[1].error = Some("unavailable".into());
    monitor.observe(failed);
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 31_000)
            .blocked
    );
    monitor.observe(sample(61_000, MemoryPressure::Normal, 100 * GIB));
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 1_000)
            .blocked
    );
    let mut invalid = sample(91_000, MemoryPressure::Normal, 100 * GIB);
    invalid.schema_version = 99;
    monitor.observe(invalid);
    assert!(monitor.snapshot().is_none());
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 91_000)
            .blocked
    );
}

#[test]
fn out_of_order_samples_do_not_release_pauses() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 31_000, MemoryPressure::Normal, GIB).blocked);
    monitor.observe(sample(1_000, MemoryPressure::Normal, 100 * GIB));
    assert_eq!(monitor.snapshot().unwrap().at_ms, 31_000);
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 31_000)
            .blocked
    );
}

#[test]
fn rotated_workspace_keeps_conservative_recovery_without_unbounded_path_history() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, GIB).blocked);
    let mut other = sample(31_000, MemoryPressure::Normal, 100 * GIB);
    other.disks[1].path = fixture_path("/other");
    monitor.observe(other);
    assert!(
        !monitor
            .assess(&policy(), &fixture_path("/other"), 31_000)
            .blocked
    );
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Normal, 28 * GIB).blocked);
    assert!(assess(&mut monitor, 91_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert!(assess(&mut monitor, 121_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert!(!assess(&mut monitor, 151_000, MemoryPressure::Normal, 40 * GIB).blocked);
    assert_eq!(monitor.disks.len(), 3);
}

#[test]
fn returning_paused_path_above_resume_still_needs_three_samples() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, GIB).blocked);
    let mut other = sample(31_000, MemoryPressure::Normal, 100 * GIB);
    other.disks[1].path = fixture_path("/other");
    monitor.observe(other);
    assert!(
        !monitor
            .assess(&policy(), &fixture_path("/other"), 31_000)
            .blocked
    );
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 91_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(!assess(&mut monitor, 121_000, MemoryPressure::Normal, 100 * GIB).blocked);
}

#[test]
fn stale_workspace_gap_resets_its_own_recovery_even_while_other_samples_are_fresh() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, GIB).blocked);
    assert!(assess(&mut monitor, 31_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Normal, 100 * GIB).blocked);
    for at_ms in [91_000, 121_000, 151_000, 181_000, 211_000] {
        let mut other = sample(at_ms, MemoryPressure::Normal, 100 * GIB);
        other.disks[1].path = fixture_path("/other");
        monitor.observe(other);
        assert!(
            !monitor
                .assess(&policy(), &fixture_path("/other"), at_ms)
                .blocked
        );
    }
    assert!(assess(&mut monitor, 241_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 271_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(!assess(&mut monitor, 301_000, MemoryPressure::Normal, 100 * GIB).blocked);
}

#[test]
fn bounded_history_never_evicts_paused_paths_and_reuses_recovered_slots() {
    let mut monitor = Monitor::new();
    for index in 0..128 {
        let at_ms = (index + 1) * 30_000;
        let path = fixture_path(&format!("/work-{index}"));
        let mut observation = sample(at_ms, MemoryPressure::Normal, GIB);
        observation.disks[1].path = path.clone();
        monitor.observe(observation);
        assert!(monitor.assess(&policy(), &path, at_ms).blocked);
    }
    assert_eq!(monitor.disks.len(), MAX_TRACKED_PATHS);
    let mut overflow = sample(3_870_000, MemoryPressure::Normal, 100 * GIB);
    overflow.disks[1].path = fixture_path("/overflow");
    monitor.observe(overflow.clone());
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/overflow"), 3_870_000)
            .blocked
    );
    assert_eq!(monitor.disks.len(), MAX_TRACKED_PATHS);
    for at_ms in [3_900_000, 3_930_000, 3_960_000] {
        let mut recovery = sample(at_ms, MemoryPressure::Normal, 100 * GIB);
        recovery.disks[1].path = fixture_path("/work-0");
        monitor.observe(recovery);
        let assessment = monitor.assess(&policy(), &fixture_path("/work-0"), at_ms);
        assert_eq!(assessment.blocked, at_ms != 3_960_000);
    }
    overflow.at_ms = 3_990_000;
    monitor.observe(overflow);
    assert!(
        !monitor
            .assess(&policy(), &fixture_path("/overflow"), 3_990_000)
            .blocked
    );
    assert_eq!(monitor.disks.len(), MAX_TRACKED_PATHS);
    assert!(!monitor.disks.contains_key(&fixture_path("/work-0")));
}

#[test]
fn sampler_failure_discards_old_good_news_and_does_not_clear_latches() {
    let mut monitor = Monitor::new();
    assert!(assess(&mut monitor, 1_000, MemoryPressure::Normal, GIB).blocked);
    assert!(assess(&mut monitor, 31_000, MemoryPressure::Normal, 100 * GIB).blocked);
    monitor.sampling_failed();
    assert!(
        monitor
            .assess(&policy(), &fixture_path("/work"), 32_000)
            .blocked
    );
    assert!(monitor.snapshot().is_none());
    assert!(assess(&mut monitor, 61_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(assess(&mut monitor, 91_000, MemoryPressure::Normal, 100 * GIB).blocked);
    assert!(!assess(&mut monitor, 121_000, MemoryPressure::Normal, 100 * GIB).blocked);
}

#[test]
fn swap_growth_is_informational_and_history_is_bounded() {
    let mut monitor = Monitor::new();
    for index in 1..=100 {
        let at_ms = index * 1_000;
        let mut observation = sample(at_ms, MemoryPressure::Normal, 100 * GIB);
        observation.memory.swap_used_bytes = Some(if index == 100 { 2 * GIB } else { 0 });
        monitor.observe(observation);
        let assessment = monitor.assess(&policy(), &fixture_path("/work"), at_ms);
        assert!(!assessment.blocked);
        if index == 100 {
            assert!(
                assessment
                    .advisories
                    .iter()
                    .any(|note| note.contains("swap"))
            );
        }
    }
    assert_eq!(monitor.swap_history.len(), MAX_HISTORY);
    let later = assess(&mut monitor, 800_000, MemoryPressure::Normal, 100 * GIB);
    assert!(!later.advisories.iter().any(|note| note.contains("swap")));
    assert_eq!(monitor.swap_history.len(), 1);
}

#[test]
fn snapshot_readback_requires_schema_bounded_paths_and_consistent_values() {
    let original = sample(1_000, MemoryPressure::Normal, 100 * GIB);
    let encoded = serde_json::to_vec(&original).unwrap();
    let decoded: Snapshot = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(original, decoded);
    assert!(decoded.validate().is_ok());
    let mut invalid = original.clone();
    invalid.disks.push(invalid.disks[0].clone());
    assert!(invalid.validate().is_err());
    invalid = original.clone();
    invalid.disks[0].total_bytes = Some(1);
    assert!(invalid.validate().is_err());
    invalid = original.clone();
    invalid.errors.push("a".repeat(MAX_ERROR_BYTES + 1));
    assert!(invalid.validate().is_err());
    invalid = original.clone();
    invalid.state_root = fixture_path("/wrong");
    assert!(invalid.validate().is_err());
    invalid = original;
    invalid.disks[1].path = "relative".into();
    assert!(invalid.validate().is_err());
}

#[test]
fn macos_parsers_distinguish_pressure_flags_and_bound_swap() {
    assert_eq!(parse_macos_pressure("1\n"), MemoryPressure::Normal);
    assert_eq!(parse_macos_pressure("2"), MemoryPressure::Warning);
    assert_eq!(parse_macos_pressure("4"), MemoryPressure::Critical);
    assert_eq!(parse_macos_pressure("3"), MemoryPressure::Unknown);
    assert_eq!(
        parse_macos_swap("total = 4096.00M  used = 1024.50M  free = 3071.50M (encrypted)"),
        Some(GIB + 512 * 1024)
    );
    assert_eq!(parse_macos_swap("used = 0.00M"), Some(0));
    assert_eq!(parse_macos_swap("used = NaNG"), None);
    assert_eq!(parse_macos_swap("used = -2.0M"), None);
    assert_eq!(parse_macos_swap("used = 9999999999999999999999G"), None);
    assert_eq!(parse_macos_swap("used = 1.0🦀"), None);
}

#[test]
fn linux_pressure_uses_available_memory_and_stalls_with_safe_unknown_fallback() {
    let healthy =
        "MemTotal: 100000 kB\nMemAvailable: 70000 kB\nSwapTotal: 1000 kB\nSwapFree: 500 kB\n";
    assert_eq!(
        parse_linux_memory(Some(healthy), None).pressure,
        MemoryPressure::Normal
    );
    assert_eq!(
        parse_linux_memory(Some(healthy), None).swap_used_bytes,
        Some(500 * 1024)
    );
    assert_eq!(
        parse_linux_memory(Some(healthy), Some("full avg10=1.50 avg60=0.2")).pressure,
        MemoryPressure::Warning
    );
    assert_eq!(
        parse_linux_memory(Some(healthy), Some("full avg10=11.00 avg60=0.2")).pressure,
        MemoryPressure::Critical
    );
    let scarce = "MemTotal: 100000 kB\nMemAvailable: 2000 kB\n";
    assert_eq!(
        parse_linux_memory(Some(scarce), Some("full avg10=0.00")).pressure,
        MemoryPressure::Critical
    );
    assert_eq!(
        parse_linux_memory(None, None).pressure,
        MemoryPressure::Unknown
    );
    assert_eq!(
        parse_linux_memory(None, Some("full avg10=NaN")).pressure,
        MemoryPressure::Unknown
    );
}

#[cfg(unix)]
#[test]
fn disk_collection_keeps_paths_and_reports_missing_directories() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("workspace");
    std::fs::create_dir(&child).unwrap();
    let missing = root.path().join("missing");
    let disks = collect_disks(&[root.path().to_path_buf(), child, missing]);
    assert_eq!(disks.len(), 3);
    assert!(disks[0].free_bytes.is_some());
    assert_eq!(disks[0].volume_id, disks[1].volume_id);
    assert_eq!(disks[0].free_bytes, disks[1].free_bytes);
    assert!(disks[2].error.is_some());
    assert!(disks[2].free_bytes.is_none());
}
