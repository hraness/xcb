use std::time::Duration;
use tokio::process::Command;
use xcb_runtime::process::{StreamProcess, capture};

#[tokio::test]
async fn finite_output_is_collected_and_oversized_output_is_refused() {
    let mut command = Command::new("/bin/echo");
    command.arg("hello").env_clear();
    assert_eq!(
        capture(command, 128, Duration::from_secs(2)).await.unwrap(),
        b"hello\n"
    );
    let mut command = Command::new("/bin/echo");
    command.arg("larger than the output allowance").env_clear();
    assert!(capture(command, 4, Duration::from_secs(2)).await.is_err());
}

#[tokio::test]
async fn a_stalled_owned_process_is_terminated_at_its_deadline() {
    let mut command = Command::new("/bin/sleep");
    command.arg("10").env_clear();
    assert!(
        capture(command, 128, Duration::from_millis(20))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cooperative_provider_exits_on_stdin_close_without_a_kill() {
    // `cat` ends on stdin EOF, so the graceful settle never reaches for the
    // process-group kill; the reaped status proves it exited on its own.
    let mut command = Command::new("/bin/cat");
    command.env_clear();
    let mut process = StreamProcess::spawn(command).unwrap();
    assert!(process.join_graceful(Duration::from_secs(2)).await);
    let status = process.exit_status().expect("child reaped");
    assert!(status.success());
}

#[tokio::test]
async fn provider_ignoring_stdin_close_still_settles_under_kill() {
    // `sleep` never reads stdin, so the grace window expires and the
    // process-group kill must still produce the same settled proof.
    let mut command = Command::new("/bin/sleep");
    command.arg("60").env_clear();
    let mut process = StreamProcess::spawn(command).unwrap();
    assert!(process.join_graceful(Duration::from_millis(200)).await);
    let status = process.exit_status().expect("child reaped");
    assert!(!status.success());
}
