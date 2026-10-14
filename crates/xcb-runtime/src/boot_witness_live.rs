//! Exercises the real macOS boot observer on CI's unsandboxed macOS runner.
//! A confined local shell may deny sysctl; failure there is not stop proof.

#[tokio::test]
async fn macos_boot_witness_is_a_stable_uuid() {
    let launch_boot = crate::managed::host_boot_identity()
        .await
        .expect("macOS boot identity must be observable for missing-PID recovery");
    let parsed = uuid::Uuid::parse_str(&launch_boot).expect("canonical boot UUID");
    assert_eq!(launch_boot, parsed.to_string());
    assert_eq!(
        launch_boot,
        crate::managed::host_boot_identity().await.unwrap(),
        "boot identity cannot drift between observation and launch"
    );
}
