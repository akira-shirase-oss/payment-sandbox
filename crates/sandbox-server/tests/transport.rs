//! Review N1: the server had no header-read or idle timeout, no connection cap and no
//! per-connection request or age limit, so silent or half-sent connections stayed open
//! (40 s and counting) until the process ran out of descriptors. The real binary is driven
//! over raw sockets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sandbox_db::testing::TestDb;

struct Server {
    child: Child,
    addr: String,
}

impl Drop for Server {
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

fn start(db: &TestDb, vars: &[(&str, &str)]) -> Server {
    let port = free_port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sandbox-server"));
    command
        .env("PS_DATABASE_URL", db.url())
        .env("PS_LISTEN", format!("127.0.0.1:{port}"))
        .env("PS_SIM_TOKEN", common::SIM_TOKEN)
        .env("PS_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in vars {
        command.env(k, v);
    }
    let server = Server {
        child: command.spawn().unwrap(),
        addr: format!("127.0.0.1:{port}"),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(mut s) = TcpStream::connect(&server.addr) {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let _ = s.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n");
            let mut buf = [0_u8; 64];
            if s.read(&mut buf)
                .is_ok_and(|n| buf[..n].starts_with(b"HTTP/1.1 200"))
            {
                return server;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the server never answered");
}

/// Reads (and discards) until the peer closes or `within` passes; true when it closed.
fn closed_within(stream: &mut TcpStream, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut buf = [0_u8; 4096];
    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return true,
        }
    }
    false
}

/// Sends one request and reads its response head; returns the head.
fn request(stream: &mut TcpStream) -> String {
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\n\r\n")
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = vec![0_u8; 4096];
    let n = stream.read(&mut buf).unwrap();
    String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase()
}

#[tokio::test]
async fn half_sent_and_idle_connections_are_closed() {
    let db = TestDb::new().await;
    let server = start(
        &db,
        &[
            ("PS_READ_TIMEOUT_MS", "500"),
            ("PS_IDLE_TIMEOUT_MS", "1500"),
        ],
    );
    // Headers never finished — the head bound is the read timeout.
    let mut partial = TcpStream::connect(&server.addr).unwrap();
    partial
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\n")
        .unwrap();
    assert!(
        closed_within(&mut partial, Duration::from_secs(4)),
        "a connection with half its headers stayed open"
    );
    // A complete request, then silence: idling past the read timeout is lawful — that
    // confusion is what closed pooled clients' sockets mid-reuse (D-103). Only the
    // idle timeout reaps a keep-alive connection, and a reuse in between is served.
    let mut idle = TcpStream::connect(&server.addr).unwrap();
    assert!(request(&mut idle).starts_with("http/1.1 200"));
    std::thread::sleep(Duration::from_millis(900));
    assert!(
        request(&mut idle).starts_with("http/1.1 200"),
        "a reuse past the read timeout found a dead socket"
    );
    assert!(
        closed_within(&mut idle, Duration::from_secs(4)),
        "an idle keep-alive connection stayed open"
    );
    // A connection that never sends a byte dies at the read bound.
    let mut silent = TcpStream::connect(&server.addr).unwrap();
    assert!(closed_within(&mut silent, Duration::from_secs(4)));
    drop(server);
    db.cleanup().await;
}

#[tokio::test]
async fn a_connection_serves_a_bounded_number_of_requests_for_a_bounded_time() {
    let db = TestDb::new().await;
    let server = start(
        &db,
        &[
            ("PS_MAX_REQUESTS_PER_CONNECTION", "3"),
            ("PS_MAX_CONNECTION_AGE_MS", "800"),
        ],
    );
    let mut stream = TcpStream::connect(&server.addr).unwrap();
    assert!(!request(&mut stream).contains("connection: close"));
    assert!(!request(&mut stream).contains("connection: close"));
    assert!(
        request(&mut stream).contains("connection: close"),
        "the third response must close the connection"
    );
    assert!(closed_within(&mut stream, Duration::from_secs(2)));

    // Age: a busy connection is told to close once it is older than its limit.
    let mut busy = TcpStream::connect(&server.addr).unwrap();
    let started = Instant::now();
    let mut closed_at = None;
    for _ in 0..4 {
        if request(&mut busy).contains("connection: close") {
            closed_at = Some(started.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let closed_at = closed_at.expect("an old connection was never told to close");
    assert!(closed_at >= Duration::from_millis(800), "{closed_at:?}");
    drop(server);
    db.cleanup().await;
}

#[tokio::test]
async fn past_the_connection_cap_connections_wait_instead_of_being_accepted() {
    let db = TestDb::new().await;
    let server = start(
        &db,
        &[("PS_MAX_CONNECTIONS", "4"), ("PS_READ_TIMEOUT_MS", "1000")],
    );
    let silent: Vec<TcpStream> = (0..4)
        .map(|_| TcpStream::connect(&server.addr).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let mut fifth = TcpStream::connect(&server.addr).unwrap();
    fifth
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .unwrap();
    fifth
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut buf = [0_u8; 64];
    let n = fifth.read(&mut buf).unwrap();
    let waited = started.elapsed();
    assert!(buf[..n].starts_with(b"HTTP/1.1 200"));
    // It was served only once the read timeout freed a place (the four silent ones were
    // closed), not at once beside them.
    assert!(
        waited >= Duration::from_millis(500),
        "the fifth connection was accepted beside four others at a cap of four: {waited:?}"
    );
    drop(silent);
    drop(server);
    db.cleanup().await;
}
