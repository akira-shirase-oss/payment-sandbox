//! Sandbox labels on the release artifact itself (M7, D-073): the binary says what it is
//! before it does anything. SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    reason = "integration tests: a panic is the failure report"
)]

#[test]
fn the_server_binary_says_it_moves_no_real_money() {
    let bin = env!("CARGO_BIN_EXE_sandbox-server");
    for args in [&["--version"][..], &["--help"][..]] {
        let output = std::process::Command::new(bin).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(sandbox_domain::SANDBOX_NOTICE),
            "{args:?}: {text}"
        );
    }
    let help = std::process::Command::new(bin)
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    for command in ["serve", "setup", "migrate", "PS_OWNER_PASSWORD"] {
        assert!(help.contains(command), "{help}");
    }
    let wrong = std::process::Command::new(bin)
        .arg("serv")
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("--help"));
}
