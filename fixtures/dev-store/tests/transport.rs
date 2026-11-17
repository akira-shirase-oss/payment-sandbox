//! Review N1: the development store had no request timeout and no read deadline, so a
//! webhook whose body stalled after 5 of 100 bytes was held open with no answer for 40 s and
//! counting, as was a half-sent request head. The real binary is driven over raw sockets.
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

struct Store(Child);

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start() -> (Store, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let db = std::env::temp_dir().join(format!("ps-store-tx-{}.redb", Uuid::now_v7().simple()));
    let store = Store(
        Command::new(env!("CARGO_BIN_EXE_sandbox-dev-store"))
            .env_clear()
            .env("PS_STORE_LISTEN", &addr)
            .env("PS_STORE_DB", db)
            .env("PS_STORE_READ_TIMEOUT_MS", "500")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(&addr).is_err() {
        assert!(Instant::now() < deadline, "the store never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    (store, addr)
}

/// Time until the store answers or closes, at most `limit`.
fn answered_within(stream: &mut TcpStream, limit: Duration) -> Option<Duration> {
    stream.set_read_timeout(Some(limit)).unwrap();
    let started = Instant::now();
    let mut buf = [0_u8; 512];
    let read = stream.read(&mut buf);
    let timed_out = read.as_ref().is_err_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    });
    (!timed_out).then(|| started.elapsed())
}

#[test]
fn a_stalled_webhook_body_and_a_half_sent_head_are_not_held_open() {
    let (_store, addr) = start();
    let mut body = TcpStream::connect(&addr).unwrap();
    body.write_all(
        b"POST /webhooks HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"id\"",
    )
    .unwrap();
    assert!(
        answered_within(&mut body, Duration::from_secs(4)).is_some(),
        "a webhook body stalled at 5 of 100 bytes got no answer"
    );
    let mut head = TcpStream::connect(&addr).unwrap();
    head.write_all(b"GET /stats HTTP/1.1\r\nHost: t\r\n")
        .unwrap();
    assert!(
        answered_within(&mut head, Duration::from_secs(4)).is_some(),
        "a half-sent head was held open"
    );
}
