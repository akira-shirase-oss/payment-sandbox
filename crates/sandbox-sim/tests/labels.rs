//! Sandbox labels on the release artifact itself (M7, D-073). SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    reason = "integration tests: a panic is the failure report"
)]

#[test]
fn the_network_binary_says_it_moves_no_real_money() {
    let bin = env!("CARGO_BIN_EXE_sandbox-sim");
    for arg in ["--version", "--help"] {
        let output = std::process::Command::new(bin).arg(arg).output().unwrap();
        assert!(output.status.success(), "{arg}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(sandbox_domain::SANDBOX_NOTICE),
            "{arg}: {text}"
        );
    }
    let wrong = std::process::Command::new(bin)
        .arg("--bogus")
        .output()
        .unwrap();
    assert!(!wrong.status.success());
}
