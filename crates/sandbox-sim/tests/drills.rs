//! The store's durability drill (D-095): the network answers only after its commit is
//! durable. The real binary is aborted after a batch is run and staged but before
//! `txn.commit()` returns — a power cut in the worst window. Afterwards: what was
//! acknowledged is recorded, what was not acknowledged never happened, and a retry under
//! the same identity executes once.
//!
//! Run with `cargo test -p sandbox-sim --features fault-injection --test drills`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

const NETWORK: &str = "drill-network-credential-0123456789";
const HOLD_MS: &str = "5000";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn store_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ps-sim-drill-{tag}-{}.redb",
        Uuid::now_v7().simple()
    ))
}

struct Sim {
    child: Child,
    base: String,
}

impl Sim {
    async fn start(path: &std::path::Path, port: u16, extra: &[(&str, &str)]) -> Self {
        let log =
            std::fs::File::create(std::env::temp_dir().join(format!("ps-sim-drill-{port}.log")))
                .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_sandbox-sim"));
        command
            .env_clear()
            .env("PS_SIM_TOKEN", NETWORK)
            .env("PS_SIM_LISTEN", format!("127.0.0.1:{port}"))
            .env("PS_SIM_DB", path)
            .env("PS_SIM_CONTROL", "off")
            .env("PS_SIM_LOST_REPLY_HOLD_MS", HOLD_MS)
            .env("PS_SIM_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(log);
        for (k, v) in extra {
            command.env(k, v);
        }
        let child = command.spawn().unwrap();
        let sim = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
        };
        sim.wait_ready().await;
        sim
    }

    async fn wait_ready(&self) {
        let http = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(r) = http.get(format!("{}/healthz", self.base)).send().await
                && r.status().is_success()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the network never became ready");
    }

    fn wait_for_crash(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    !status.success(),
                    "the network exited cleanly instead of crashing"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the fault point never fired");
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One `POST /v1/operations`: the status, or `None` when the request never got an answer.
async fn execute(base: &str, id: Uuid) -> Option<u16> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let body = json!({
        "id": id,
        "project_id": Uuid::from_u128(1),
        "kind": "authorize",
        "parent_id": null,
        "amount_minor": 500,
        "currency": "USD",
        "token": "tok_sim_approve",
    });
    http.post(format!("{base}/v1/operations"))
        .bearer_auth(NETWORK)
        .header("sandbox-network-protocol", "sim/v1")
        .json(&body)
        .send()
        .await
        .ok()
        .map(|r| r.status().as_u16())
}

async fn readback(base: &str, id: Uuid) -> Value {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    http.get(format!("{base}/v1/operations/{id}"))
        .bearer_auth(NETWORK)
        .header("sandbox-network-protocol", "sim/v1")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()
}

/// A crash between batching and commit loses no acknowledged command and records no
/// unacknowledged one: the first two executions (their own batches) commit and answer;
/// the third batch is aborted before its commit, so its caller can never have been told.
#[tokio::test]
async fn a_crash_between_batching_and_commit_loses_no_acknowledged_command() {
    let file = store_path("commit");
    let port = free_port();
    let committed: Vec<Uuid> = (0..2).map(|_| Uuid::now_v7()).collect();
    let lost = Uuid::now_v7();

    let mut sim = Sim::start(
        &file,
        port,
        &[
            ("PS_SIM_FAULT", "store_commit"),
            ("PS_SIM_FAULT_AFTER", "3"),
        ],
    )
    .await;
    for id in &committed {
        assert_eq!(
            execute(&sim.base, *id).await,
            Some(200),
            "an early commit must answer"
        );
    }
    // The third batch is aborted before its commit: whatever the caller saw, it was not a
    // recorded answer.
    assert_ne!(
        execute(&sim.base, lost).await,
        Some(200),
        "a crash before the commit must not acknowledge"
    );
    sim.wait_for_crash();

    // A fresh process on the same file: the acknowledged executions are durable; the
    // unacknowledged one was never recorded.
    let sim = Sim::start(&file, port, &[]).await;
    for id in &committed {
        let body = readback(&sim.base, *id).await;
        assert_eq!(
            body["status"].as_str().unwrap_or("?"),
            "executed",
            "an acknowledged command was lost: {body}"
        );
    }
    let body = readback(&sim.base, lost).await;
    assert_eq!(
        body["status"].as_str().unwrap_or("?"),
        "unknown",
        "an unacknowledged command was recorded: {body}"
    );

    // The client's retry under the same identity executes once — never twice.
    assert_eq!(execute(&sim.base, lost).await, Some(200));
    let body = readback(&sim.base, lost).await;
    assert_eq!(body["status"].as_str().unwrap_or("?"), "executed");
    assert_eq!(execute(&sim.base, lost).await, Some(200));
    sim.stop();
    let _ = std::fs::remove_file(file);
}
