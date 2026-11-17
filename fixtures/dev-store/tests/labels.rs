//! The development store says what it is and exits on `--version` and `--help`, instead of
//! starting a store (v1 review L8: `--version` started one on 56082 and wrote its file in
//! the current directory). SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::time::{Duration, Instant};

#[test]
fn version_and_help_answer_and_exit_without_starting_a_store() {
    let bin = env!("CARGO_BIN_EXE_sandbox-dev-store");
    let dir = std::env::temp_dir().join(format!("ps-store-labels-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for arg in ["--version", "-V", "--help", "-h"] {
        let mut child = std::process::Command::new(bin)
            .arg(arg)
            .current_dir(&dir)
            .env_remove("PS_STORE_DB")
            // If it started anyway, it would hold a port of the test range, not 56082.
            .env("PS_STORE_LISTEN", "127.0.0.1:56499")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if started.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let output = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            status.is_some_and(|s| s.success()),
            "{arg}: still running after 5 s, or failed ({status:?}): it started a store"
        );
        assert!(
            text.contains(sandbox_domain::SANDBOX_NOTICE),
            "{arg}: {text}"
        );
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(files.is_empty(), "{arg} wrote {files:?}");
    }
    let wrong = std::process::Command::new(bin)
        .arg("--bogus")
        .current_dir(&dir)
        .env("PS_STORE_LISTEN", "127.0.0.1:56499")
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}
