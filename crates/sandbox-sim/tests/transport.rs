//! Review N1: under a 64-descriptor limit, 70 silent connections stopped the simulator
//! accepting (every later accept failed with "too many open files") until they closed by
//! themselves. Now connections past a cap wait in the listen backlog, and a connection that
//! does not finish its request head within the read timeout is closed, so the network keeps
//! answering its real client.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use uuid::Uuid;

const NETWORK: &str = "network-credential-for-tests-0123456789";

struct Sim(Child);

impl Drop for Sim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn silent_connections_cannot_exhaust_the_networks_descriptors() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let db = std::env::temp_dir().join(format!("ps-sim-fd-{}.redb", Uuid::now_v7().simple()));
    // `exec` keeps the limit on the simulator itself.
    let sim = Sim(Command::new("sh")
        .arg("-c")
        .arg(format!(
            "ulimit -n 64 && exec {}",
            env!("CARGO_BIN_EXE_sandbox-sim")
        ))
        .env_clear()
        .env("PS_SIM_LISTEN", &addr)
        .env("PS_SIM_DB", &db)
        .env("PS_SIM_TOKEN", NETWORK)
        .env("PS_SIM_LOG", "error")
        .env("PS_SIM_MAX_CONNECTIONS", "24")
        .env("PS_SIM_READ_TIMEOUT_MS", "1000")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap());
    // No pooled connection: every attempt needs the network to accept a new one.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let list = format!("http://{addr}/v1/operations?limit=1");
    let ready = Instant::now() + Duration::from_secs(30);
    while http.get(&list).bearer_auth(NETWORK).send().await.is_err() {
        assert!(Instant::now() < ready, "the simulator never answered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 70 connections that never send a byte, held open by the client.
    let silent: Vec<TcpStream> = (0..70)
        .filter_map(|_| TcpStream::connect(&addr).ok())
        .collect();
    assert!(silent.len() >= 60, "{}", silent.len());
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    let mut answered = false;
    while started.elapsed() < Duration::from_secs(12) {
        if let Ok(r) = http.get(&list).bearer_auth(NETWORK).send().await
            && r.status().is_success()
        {
            answered = true;
            break;
        }
    }
    let waited = started.elapsed();
    assert!(
        answered && waited < Duration::from_secs(10),
        "the network stopped answering its client while silent connections were held: {waited:?}"
    );
    drop(silent);
    drop(sim);
    let _ = std::fs::remove_file(db);
}

#[test]
fn a_half_sent_request_head_is_closed() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let db = std::env::temp_dir().join(format!("ps-sim-hdr-{}.redb", Uuid::now_v7().simple()));
    let _sim = Sim(Command::new(env!("CARGO_BIN_EXE_sandbox-sim"))
        .env_clear()
        .env("PS_SIM_LISTEN", &addr)
        .env("PS_SIM_DB", &db)
        .env("PS_SIM_TOKEN", NETWORK)
        .env("PS_SIM_READ_TIMEOUT_MS", "500")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stream = loop {
        if let Ok(s) = TcpStream::connect(&addr) {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    };
    stream
        .write_all(b"POST /v1/operations HTTP/1.1\r\nHost: t\r\n")
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let started = Instant::now();
    let mut buf = [0_u8; 256];
    let _ = stream.read(&mut buf);
    let held = started.elapsed();
    assert!(
        held < Duration::from_secs(4),
        "a half-sent head was held open for {held:?}"
    );
    let _ = std::fs::remove_file(db);
}
