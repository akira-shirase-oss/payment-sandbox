//! Review finding 10: the network's routes are not open to whoever can reach the port. The
//! real binary is started the way an operator would start it. Executions, readbacks (and so
//! fences) and listings need the network credential the server holds; the test control
//! plane (faults, the virtual clock) is not mounted at all unless the startup flag turns it
//! on, which only the `dev` profile does by default, and then it needs its own credential.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use uuid::Uuid;

const NETWORK: &str = "network-credential-for-tests-0123456789";
const CONTROL: &str = "control-credential-for-tests-0123456789";

struct Sim {
    child: Child,
    base: String,
}

impl Drop for Sim {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn command(vars: &[(&str, &str)]) -> (Command, String) {
    let port = free_port();
    let db = std::env::temp_dir().join(format!("ps-sim-access-{}.redb", Uuid::now_v7().simple()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_sandbox-sim"));
    command
        .env_clear()
        .env("PS_SIM_LISTEN", format!("127.0.0.1:{port}"))
        .env("PS_SIM_DB", db)
        .env("PS_SIM_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in vars {
        command.env(k, v);
    }
    (command, format!("http://127.0.0.1:{port}"))
}

async fn start(vars: &[(&str, &str)]) -> Sim {
    let (mut command, base) = command(vars);
    let child = command.spawn().unwrap();
    let sim = Sim { child, base };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(r) = http.get(format!("{}/healthz", sim.base)).send().await
            && r.status().is_success()
        {
            return sim;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the simulator never answered /healthz");
}

/// Waits for a process that must refuse to start; true when it exited with a failure.
fn refuses_to_start(vars: &[(&str, &str)]) -> bool {
    let (mut command, _) = command(vars);
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return !status.success();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

fn authorize() -> serde_json::Value {
    json!({
        "id": Uuid::now_v7(),
        "project_id": Uuid::now_v7(),
        "kind": "authorize",
        "parent_id": null,
        "amount_minor": 500,
        "currency": "USD",
        "token": "tok_sim_approve",
    })
}

async fn status(request: reqwest::RequestBuilder) -> u16 {
    request.send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn the_network_needs_its_credential_and_the_control_plane_is_off_outside_dev() {
    let sim = start(&[("PS_SIM_TOKEN", NETWORK)]).await;
    let http = reqwest::Client::new();
    let ops = format!("{}/v1/operations", sim.base);
    let one = format!("{}/v1/operations/{}?fence=true", sim.base, Uuid::now_v7());

    // No credential, or the wrong one: nothing executes, nothing is fenced or listed.
    for bearer in [
        None,
        Some("wrong-credential-0123456789abcdef0123"),
        Some(CONTROL),
    ] {
        let with = |r: reqwest::RequestBuilder| match bearer {
            Some(token) => r.bearer_auth(token),
            None => r,
        };
        assert_eq!(status(with(http.post(&ops).json(&authorize()))).await, 401);
        assert_eq!(status(with(http.get(&one))).await, 401);
        assert_eq!(status(with(http.get(&ops))).await, 401);
    }
    // The server's credential works.
    assert_eq!(
        status(http.post(&ops).bearer_auth(NETWORK).json(&authorize())).await,
        200
    );
    assert_eq!(status(http.get(&one).bearer_auth(NETWORK)).await, 200);

    // Outside the dev profile the control plane does not exist, whatever credential.
    for bearer in [NETWORK, CONTROL] {
        let faults = http
            .post(format!("{}/v1/faults", sim.base))
            .bearer_auth(bearer)
            .json(&json!({"kind": null, "target": null, "action": "unavailable", "count": 1}));
        assert_eq!(status(faults).await, 404);
        let clock = http
            .post(format!("{}/v1/clock", sim.base))
            .bearer_auth(bearer)
            .json(&json!({"seconds": 86_400}));
        assert_eq!(status(clock).await, 404);
        let read = http
            .get(format!("{}/v1/clock", sim.base))
            .bearer_auth(bearer);
        assert_eq!(status(read).await, 404);
    }
}

#[tokio::test]
async fn in_the_dev_profile_the_control_plane_needs_its_own_credential() {
    let sim = start(&[
        ("PS_SIM_TOKEN", NETWORK),
        ("PS_PROFILE", "dev"),
        ("PS_SIM_CONTROL_TOKEN", CONTROL),
    ])
    .await;
    let http = reqwest::Client::new();
    let clock = format!("{}/v1/clock", sim.base);
    let faults = format!("{}/v1/faults", sim.base);
    let rule = json!({"kind": null, "target": null, "action": "unavailable", "count": 1});
    // The network credential is not the operator's.
    assert_eq!(status(http.get(&clock)).await, 401);
    assert_eq!(status(http.get(&clock).bearer_auth(NETWORK)).await, 401);
    assert_eq!(
        status(http.post(&faults).bearer_auth(NETWORK).json(&rule)).await,
        401
    );
    assert_eq!(status(http.get(&clock).bearer_auth(CONTROL)).await, 200);
    assert_eq!(
        status(
            http.post(&clock)
                .bearer_auth(CONTROL)
                .json(&json!({"seconds": 60}))
        )
        .await,
        200
    );
    assert_eq!(
        status(http.post(&faults).bearer_auth(CONTROL).json(&rule)).await,
        201
    );
    // And the control credential does not execute operations.
    assert_eq!(
        status(
            http.post(format!("{}/v1/operations", sim.base))
                .bearer_auth(CONTROL)
                .json(&authorize())
        )
        .await,
        401
    );
}

#[test]
fn it_refuses_to_start_without_credentials_or_with_a_control_plane_and_no_credential() {
    // No network credential at all.
    assert!(refuses_to_start(&[]));
    // Too short to be a credential.
    assert!(refuses_to_start(&[("PS_SIM_TOKEN", "short")]));
    // The control plane switched on explicitly, but with no credential of its own.
    assert!(refuses_to_start(&[
        ("PS_SIM_TOKEN", NETWORK),
        ("PS_SIM_CONTROL", "on")
    ]));
    // The dev profile turns it on by default, so it needs the credential too.
    assert!(refuses_to_start(&[
        ("PS_SIM_TOKEN", NETWORK),
        ("PS_PROFILE", "dev")
    ]));
    // One credential cannot be both.
    assert!(refuses_to_start(&[
        ("PS_SIM_TOKEN", NETWORK),
        ("PS_PROFILE", "dev"),
        ("PS_SIM_CONTROL_TOKEN", NETWORK),
    ]));
    // An unknown profile is a typo, not a default.
    assert!(refuses_to_start(&[
        ("PS_SIM_TOKEN", NETWORK),
        ("PS_PROFILE", "prod")
    ]));
}
