//! The `sandbox-cli` binary against a real sandbox (M7, D-068): help that says what to do,
//! production-looking configuration refused before anything is sent, every exit status
//! meaning what `--help` says, a typed reset, and retries that can never charge twice.
//! SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::Stack;
use sandbox_cli::{Client, ClientError, Config, RetryPolicy};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Runs the binary with `args`, the API at `api` and `key`; stdin is not a terminal.
async fn cli(api: &str, key: Option<&str>, args: &[&str]) -> Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_sandbox-cli"));
    command
        .args(args)
        .env_remove("PS_API_KEY")
        .env_remove("PS_ALLOW_NON_LOCAL")
        .env("PS_API_URL", api)
        .stdin(std::process::Stdio::null());
    if let Some(key) = key {
        command.env("PS_API_KEY", key);
    }
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json_out(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not JSON: {}", stdout(output)))
}

const NOTICE: &str = "SIMULATED MONEY ONLY";

#[tokio::test]
async fn help_and_version_carry_the_notice_and_say_what_each_exit_status_means() {
    let help = cli("http://127.0.0.1:1", None, &["--help"]).await;
    assert_eq!(code(&help), 0);
    let text = stdout(&help);
    for needle in [
        NOTICE,
        "Exit status:",
        "4  the outcome is not known yet",
        "payments",
        "scenarios",
        "reset",
        "Examples:",
    ] {
        assert!(text.contains(needle), "--help lacks {needle:?}:\n{text}");
    }
    let version = cli("http://127.0.0.1:1", None, &["--version"]).await;
    assert!(stdout(&version).contains("sandbox-cli"));
    let long = cli("http://127.0.0.1:1", None, &["-V"]).await;
    assert_eq!(code(&long), 0);
    let create = cli(
        "http://127.0.0.1:1",
        None,
        &["payments", "create", "--help"],
    )
    .await;
    let text = stdout(&create);
    assert!(text.contains("42.00 USD, 500 JPY, 1.250 KWD"), "{text}");
    assert!(text.contains("lost_reply"), "{text}");
    // A typo gets a suggestion and exit status 2.
    let typo = cli("http://127.0.0.1:1", None, &["paymnts"]).await;
    assert_eq!(code(&typo), 2);
    assert!(stderr(&typo).contains("payments"), "{}", stderr(&typo));
}

#[tokio::test]
async fn production_looking_configuration_is_refused_before_anything_is_sent() {
    let key = "sk_sim_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    // Built at run time, so no literal in the source looks like a real processor's key.
    let live = format!("sk_{}_{}", "live", "0123456789abcdef".repeat(4));
    let live = live.as_str();
    let refused = cli("http://127.0.0.1:56080", Some(live), &["status"]).await;
    assert_eq!(code(&refused), 2);
    assert!(stderr(&refused).contains("sk_sim_"), "{}", stderr(&refused));
    assert!(!stderr(&refused).contains(live), "the key is never echoed");
    assert!(stderr(&refused).starts_with(NOTICE), "{}", stderr(&refused));

    let stripe = cli(
        "https://api.stripe.com",
        Some(key),
        &["--allow-non-local", "status"],
    )
    .await;
    assert_eq!(code(&stripe), 2);
    assert!(
        stderr(&stripe).contains("real payment processor"),
        "{}",
        stderr(&stripe)
    );

    let remote = cli("https://sandbox.example.com", Some(key), &["status"]).await;
    assert_eq!(code(&remote), 2);
    assert!(
        stderr(&remote).contains("--allow-non-local"),
        "{}",
        stderr(&remote)
    );

    let missing = cli("http://127.0.0.1:56080", None, &["status"]).await;
    assert_eq!(code(&missing), 2);
    assert!(
        stderr(&missing).contains("PS_API_KEY"),
        "{}",
        stderr(&missing)
    );

    // Nothing listens on port 1: unreachable, with what to check.
    let down = cli(
        "http://127.0.0.1:1",
        Some(key),
        &["--timeout", "2", "status"],
    )
    .await;
    assert_eq!(code(&down), 5);
    assert!(
        stderr(&down).contains("sandbox-server is running"),
        "{}",
        stderr(&down)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one developer's session, in order")]
async fn a_developer_session_from_the_command_line() {
    let stack = Stack::start().await;
    let (slug, admin, read) = stack.project("cli-shop").await;
    let api = stack.api.as_str();

    let status = cli(api, Some(&admin), &["status"]).await;
    assert_eq!(code(&status), 0, "{}", stderr(&status));
    assert!(
        stdout(&status).contains("dataset epoch 1"),
        "{}",
        stdout(&status)
    );

    // A purchase: authorize, capture, partial refund; the capacity is never overrun.
    let created = cli(
        api,
        Some(&admin),
        &[
            "--json",
            "payments",
            "create",
            "--amount",
            "42.00",
            "--currency",
            "usd",
        ],
    )
    .await;
    assert_eq!(code(&created), 0, "{}", stderr(&created));
    let payment = json_out(&created)["payment"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let captured = cli(
        api,
        Some(&admin),
        &["payments", "capture", &payment, "--amount", "42.00"],
    )
    .await;
    assert_eq!(code(&captured), 0, "{}", stderr(&captured));
    assert!(
        stdout(&captured).contains("capture of 42.00 USD succeeded"),
        "{}",
        stdout(&captured)
    );
    assert!(
        stdout(&captured).contains("Idempotency-Key cli_"),
        "{}",
        stdout(&captured)
    );
    let refunded = cli(
        api,
        Some(&admin),
        &["payments", "refund", &payment, "--amount", "12.00"],
    )
    .await;
    assert_eq!(code(&refunded), 0, "{}", stderr(&refunded));
    let too_much = cli(
        api,
        Some(&admin),
        &["payments", "refund", &payment, "--amount", "30.01"],
    )
    .await;
    assert_eq!(code(&too_much), 1);
    assert!(
        stderr(&too_much).contains("insufficient_capacity"),
        "{}",
        stderr(&too_much)
    );
    let bad_amount = cli(
        api,
        Some(&admin),
        &["payments", "refund", &payment, "--amount", "1.234"],
    )
    .await;
    assert_eq!(code(&bad_amount), 2, "{}", stderr(&bad_amount));
    assert!(
        stderr(&bad_amount).contains("write it like 42.00"),
        "{}",
        stderr(&bad_amount)
    );

    // The same key again replays; reusing it for another amount is a conflict.
    let key_args = [
        "payments",
        "create",
        "--amount",
        "5.00",
        "--currency",
        "EUR",
        "--idempotency-key",
        "my-order-17",
    ];
    let first = cli(api, Some(&admin), &key_args).await;
    assert_eq!(code(&first), 0, "{}", stderr(&first));
    let again = cli(api, Some(&admin), &key_args).await;
    assert_eq!(code(&again), 0);
    assert!(stdout(&again).contains("a repeat"), "{}", stdout(&again));
    let other = cli(
        api,
        Some(&admin),
        &[
            "payments",
            "create",
            "--amount",
            "6.00",
            "--currency",
            "EUR",
            "--idempotency-key",
            "my-order-17",
        ],
    )
    .await;
    assert_eq!(code(&other), 1);
    assert!(
        stderr(&other).contains("idempotency_key_conflict"),
        "{}",
        stderr(&other)
    );

    // A decline is exit 3; an uncertain answer is exit 4 and never called done.
    let declined = cli(
        api,
        Some(&admin),
        &[
            "payments",
            "create",
            "--amount",
            "9.99",
            "--currency",
            "USD",
            "--scenario",
            "decline_insufficient_funds",
        ],
    )
    .await;
    assert_eq!(code(&declined), 3, "{}", stdout(&declined));
    assert!(
        stdout(&declined).contains("insufficient_funds"),
        "{}",
        stdout(&declined)
    );
    let lost = cli(
        api,
        Some(&admin),
        &[
            "--json",
            "payments",
            "create",
            "--amount",
            "7.00",
            "--currency",
            "USD",
            "--scenario",
            "lost_reply",
        ],
    )
    .await;
    assert_eq!(code(&lost), 4, "{}", stdout(&lost));
    let lost_id = json_out(&lost)["payment"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let words = cli(api, Some(&admin), &["payments", "get", &lost_id]).await;
    assert!(stdout(&words).contains("uncertain") || stdout(&words).contains("succeeded"));
    let waited = cli(
        api,
        Some(&admin),
        &["payments", "wait", &lost_id, "--wait-secs", "30"],
    )
    .await;
    assert_eq!(code(&waited), 0, "{}", stdout(&waited));
    let line = stdout(&waited)
        .lines()
        .find(|l| l.trim_start().starts_with("authorize "))
        .unwrap()
        .to_owned();
    assert!(
        line.contains("7.00 USD") && line.ends_with("succeeded"),
        "{line}"
    );

    // The ledger explains itself; reconciliation is clean, also against the network.
    let explained = cli(
        api,
        Some(&read),
        &[
            "ledger",
            "explain",
            "USD",
            "merchant_pending",
            "--page",
            "1",
        ],
    )
    .await;
    assert_eq!(code(&explained), 0, "{}", stderr(&explained));
    assert!(
        stdout(&explained).contains("Explained: the lines sum to the balance, 30.00 USD"),
        "{}",
        stdout(&explained)
    );
    let balances = cli(api, Some(&read), &["ledger", "balances"]).await;
    assert!(
        stdout(&balances).contains("merchant_pending"),
        "{}",
        stdout(&balances)
    );
    let journal = cli(api, Some(&read), &["ledger", "journal", &payment]).await;
    assert!(
        stdout(&journal).contains("refund USD"),
        "{}",
        stdout(&journal)
    );
    let clean = cli(api, Some(&read), &["reconcile", "--network"]).await;
    assert_eq!(code(&clean), 0, "{}", stdout(&clean));
    assert!(stdout(&clean).starts_with("Clean"), "{}", stdout(&clean));
    let events = cli(api, Some(&read), &["events", "--payment", &payment]).await;
    assert!(
        stdout(&events).contains("refund.succeeded"),
        "{}",
        stdout(&events)
    );

    // The scenario library runs and checks itself.
    let library = cli(api, Some(&admin), &["scenarios", "list"]).await;
    assert!(
        stdout(&library).contains("lost-reply"),
        "{}",
        stdout(&library)
    );
    let checked = cli(
        api,
        Some(&admin),
        &[
            "scenarios",
            "run",
            "purchase",
            "network-unavailable",
            "--check",
        ],
    )
    .await;
    assert_eq!(
        code(&checked),
        0,
        "{}\n{}",
        stdout(&checked),
        stderr(&checked)
    );
    assert_eq!(
        stdout(&checked)
            .matches("matches the library's expected trace")
            .count(),
        2
    );
    let unknown = cli(api, Some(&admin), &["scenarios", "run", "teleport"]).await;
    assert_eq!(code(&unknown), 2);
    assert!(
        stderr(&unknown).contains("sandbox-cli scenarios list"),
        "{}",
        stderr(&unknown)
    );

    // Reset: the slug typed, admin only, nothing deleted.
    let untyped = cli(api, Some(&admin), &["reset"]).await;
    assert_eq!(code(&untyped), 2);
    assert!(
        stderr(&untyped).contains(&format!("--confirm {slug}")),
        "{}",
        stderr(&untyped)
    );
    let wrong = cli(api, Some(&admin), &["reset", "--confirm", "cli-sho"]).await;
    assert_eq!(code(&wrong), 1);
    assert!(
        stderr(&wrong).contains("nothing was reset"),
        "{}",
        stderr(&wrong)
    );
    let reader = cli(api, Some(&read), &["reset", "--confirm", &slug]).await;
    assert_eq!(code(&reader), 1);
    assert!(stderr(&reader).contains("admin"), "{}", stderr(&reader));
    let reset = cli(api, Some(&admin), &["reset", "--confirm", &slug]).await;
    assert_eq!(code(&reset), 0, "{}", stderr(&reset));
    assert!(
        stdout(&reset).contains("epoch 2 is now current"),
        "{}",
        stdout(&reset)
    );
    let after = cli(api, Some(&admin), &["status"]).await;
    assert!(
        stdout(&after).contains("dataset epoch 2"),
        "{}",
        stdout(&after)
    );
    let empty = cli(api, Some(&admin), &["payments", "list"]).await;
    assert!(
        stdout(&empty).contains("No payments in this epoch yet"),
        "{}",
        stdout(&empty)
    );
    let archived = cli(api, Some(&admin), &["payments", "get", &payment]).await;
    assert_eq!(code(&archived), 1);
    assert!(
        stderr(&archived).contains("dataset_archived"),
        "{}",
        stderr(&archived)
    );
    stack.stop().await;
}

/// A proxy in front of the API that loses the response to the first `POST /v1/payments`:
/// it forwards the request, reads the server's whole answer, then drops the connection.
async fn lossy_proxy(upstream: String) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let lost = Arc::new(AtomicUsize::new(0));
    let counter = lost.clone();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let (upstream, counter) = (upstream.clone(), counter.clone());
            tokio::spawn(async move {
                let addr = upstream.trim_start_matches("http://");
                let mut server = tokio::net::TcpStream::connect(addr).await.unwrap();
                loop {
                    let Some(request) = read_message(&mut client).await else {
                        return;
                    };
                    server.write_all(&request).await.unwrap();
                    let Some(response) = read_message(&mut server).await else {
                        return;
                    };
                    let is_create = request.starts_with(b"POST /v1/payments ");
                    if is_create && counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        return; // the answer is lost: the connection just closes
                    }
                    client.write_all(&response).await.unwrap();
                }
            });
        }
    });
    (url, lost)
}

/// One HTTP/1.1 message (head and a Content-Length body), or `None` at end of stream.
async fn read_message(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse().unwrap());
            while buffer.len() < end + 4 + length {
                let n = stream.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            return Some(buffer);
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_response_is_retried_under_the_same_key_and_charges_once() {
    let stack = Stack::start().await;
    let (_, admin, _) = stack.project("lossy").await;
    let (proxy, lost) = lossy_proxy(stack.api.clone()).await;
    let client = Client::new(
        Config::new(&proxy, Some(&admin), false).unwrap(),
        RetryPolicy::default(),
    )
    .unwrap();
    let answer = client
        .post_money(
            "/v1/payments",
            &json!({"amount": 1234, "currency": "USD", "token": "tok_sim_approve"}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        lost.load(Ordering::SeqCst),
        2,
        "one answer lost, one delivered"
    );
    assert_eq!(answer.attempts, 2);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body["replayed"], true,
        "the retry replayed the first request"
    );
    let direct = Client::new(
        Config::new(&stack.api, Some(&admin), false).unwrap(),
        RetryPolicy::default(),
    )
    .unwrap();
    let payments = direct.get("/v1/payments").await.unwrap();
    assert_eq!(
        payments.body["payments"].as_array().unwrap().len(),
        1,
        "charged once"
    );
    stack.stop().await;
}

/// A fake sandbox answering `statuses` in turn (then the last forever), recording the
/// idempotency key of every attempt.
async fn fake(statuses: Vec<(u16, Value, bool)>) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    use axum::http::{HeaderMap, StatusCode};
    let keys = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = keys.clone();
    let turn = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |headers: HeaderMap| {
        let (seen, turn, statuses) = (seen.clone(), turn.clone(), statuses.clone());
        async move {
            let key = headers
                .get("idempotency-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            seen.lock().unwrap().push(key);
            let n = turn.fetch_add(1, Ordering::SeqCst).min(statuses.len() - 1);
            let (status, body, simulated) = statuses[n].clone();
            let mut response =
                (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response();
            if simulated {
                response
                    .headers_mut()
                    .insert("sandbox-simulated", "true".parse().unwrap());
            }
            response
                .headers_mut()
                .insert("retry-after", "0".parse().unwrap());
            response
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, keys)
}

use axum::response::IntoResponse as _;

fn fast() -> RetryPolicy {
    RetryPolicy {
        base: Duration::from_millis(5),
        cap: Duration::from_millis(20),
        ..RetryPolicy::default()
    }
}

const KEY: &str = "sk_sim_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn busy_answers_are_retried_under_the_same_key_and_the_retries_are_bounded() {
    let busy = json!({"error": {"code": "payment_busy", "message": "busy"}, "simulated": true});
    let done = json!({"operation": {"state": "succeeded"}, "simulated": true});
    let (url, keys) = fake(vec![
        (503, busy.clone(), true),
        (503, busy.clone(), true),
        (201, done, true),
    ])
    .await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    let answer = client
        .post_money("/v1/payments/x/captures", &json!({"amount": 1}), None)
        .await
        .unwrap();
    assert_eq!((answer.status, answer.attempts), (201, 3));
    let seen = keys.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert!(
        seen.iter().all(|k| k == &seen[0] && k.starts_with("cli_")),
        "{seen:?}"
    );

    // Always busy: four attempts, then an honest "unknown" naming the key to retry with.
    let (url, keys) = fake(vec![(503, busy, true)]).await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    let error = client
        .post_money("/v1/payments", &json!({}), Some("order-9".to_owned()))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ClientError::Unknown { idempotency_key, attempts: 4, .. } if idempotency_key == "order-9"),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("--idempotency-key order-9"),
        "{error}"
    );
    assert_eq!(keys.lock().unwrap().len(), 4);

    // A conflict is final: one attempt, no retry.
    let conflict = json!({"error": {"code": "idempotency_key_conflict", "message": "reused"}, "simulated": true});
    let (url, keys) = fake(vec![(409, conflict, true)]).await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    let error = client
        .post_money("/v1/payments", &json!({}), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some("idempotency_key_conflict"));
    assert_eq!(keys.lock().unwrap().len(), 1);

    // Something that is not a sandbox: the client stops at once.
    let (url, _) = fake(vec![(200, json!({"ok": true}), false)]).await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    assert!(matches!(
        client.get("/v1/project").await.unwrap_err(),
        ClientError::NotASandbox { .. }
    ));
}

/// A server error or a gateway's answer on a keyed (money-moving) request is "no answer":
/// the request may have been applied, so it is retried under the same key, and if the
/// retries run out the outcome is unknown, naming the key (D-075). Never "refused" (that
/// tells a store to start over with a new key: a second charge), never "not a sandbox".
#[tokio::test]
async fn server_errors_and_gateway_answers_on_money_moving_calls_are_unknown_not_refused() {
    let internal =
        json!({"error": {"code": "internal", "message": "internal error"}, "simulated": true});
    let done = json!({"operation": {"state": "succeeded"}, "replayed": true, "simulated": true});
    for (label, first) in [
        ("500 from the sandbox", (500, internal.clone(), true)),
        ("502 from a proxy", (502, Value::Null, false)),
        ("504 from a proxy", (504, Value::Null, false)),
        ("503 from a proxy", (503, Value::Null, false)),
    ] {
        // Once, then the sandbox answers: retried under the same key.
        let (url, keys) = fake(vec![first.clone(), (200, done.clone(), true)]).await;
        let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
        let answer = client
            .post_money("/v1/payments", &json!({"amount": 1}), None)
            .await
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!((answer.status, answer.attempts), (200, 2), "{label}");
        let seen = keys.lock().unwrap().clone();
        assert!(seen.len() == 2 && seen[0] == seen[1], "{label}: {seen:?}");

        // Every time: four attempts, then unknown, naming the key.
        let (url, keys) = fake(vec![first]).await;
        let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
        let error = client
            .post_money("/v1/payments", &json!({}), Some("order-7".to_owned()))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ClientError::Unknown { idempotency_key, attempts: 4, .. } if idempotency_key == "order-7"),
            "{label}: {error:?}"
        );
        assert!(
            error.to_string().contains("--idempotency-key order-7"),
            "{label}: {error}"
        );
        assert_eq!(keys.lock().unwrap().len(), 4, "{label}");
    }

    // A read through a gateway that answers 502 is retried, then "unreachable", not "not a
    // sandbox".
    let (url, keys) = fake(vec![(502, Value::Null, false)]).await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    let error = client.get("/v1/project").await.unwrap_err();
    assert!(
        matches!(&error, ClientError::Unreachable { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("502"), "{error}");
    assert_eq!(keys.lock().unwrap().len(), 4);
    // Something that answers 4xx without the header is still not a sandbox.
    let (url, _) = fake(vec![(404, Value::Null, false)]).await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    assert!(matches!(
        client
            .post_money("/v1/payments", &json!({}), None)
            .await
            .unwrap_err(),
        ClientError::NotASandbox { .. }
    ));

    // The binary: exit status 4 and the key to retry with.
    let (url, _) = fake(vec![(502, Value::Null, false)]).await;
    let output = cli(
        &url,
        Some(KEY),
        &[
            "--timeout",
            "5",
            "payments",
            "create",
            "--amount",
            "1.00",
            "--currency",
            "USD",
            "--idempotency-key",
            "order-8",
        ],
    )
    .await;
    assert_eq!(code(&output), 4, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("--idempotency-key order-8"),
        "{}",
        stderr(&output)
    );
}

/// A proxy in front of the API while the server restarts in the middle of a payment: the
/// first `POST /v1/payments` reaches the server, which records it, but the proxy answers
/// `502` without the sandbox header (its upstream went away); the next attempt finds the
/// server still down (`502` at once); then it is back.
async fn restarting_proxy(upstream: String) -> (String, Arc<AtomicUsize>) {
    const BAD_GATEWAY: &[u8] =
        b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let creates = Arc::new(AtomicUsize::new(0));
    let counter = creates.clone();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let (upstream, counter) = (upstream.clone(), counter.clone());
            tokio::spawn(async move {
                let addr = upstream.trim_start_matches("http://");
                loop {
                    let Some(request) = read_message(&mut client).await else {
                        return;
                    };
                    let turn = if request.starts_with(b"POST /v1/payments ") {
                        counter.fetch_add(1, Ordering::SeqCst)
                    } else {
                        usize::MAX
                    };
                    if turn == 1 {
                        // The server is still restarting: nothing reaches it.
                        let _ = client.write_all(BAD_GATEWAY).await;
                        return;
                    }
                    let mut server = tokio::net::TcpStream::connect(addr).await.unwrap();
                    server.write_all(&request).await.unwrap();
                    let Some(response) = read_message(&mut server).await else {
                        return;
                    };
                    if turn == 0 {
                        // The request was applied; its answer died with the server.
                        let _ = client.write_all(BAD_GATEWAY).await;
                        return;
                    }
                    client.write_all(&response).await.unwrap();
                }
            });
        }
    });
    (url, creates)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_restart_behind_a_proxy_mid_payment_charges_once() {
    let stack = Stack::start().await;
    let (_, admin, _) = stack.project("restart").await;
    let (proxy, creates) = restarting_proxy(stack.api.clone()).await;
    let client = Client::new(
        Config::new(&proxy, Some(&admin), false).unwrap(),
        RetryPolicy {
            base: Duration::from_millis(20),
            cap: Duration::from_millis(100),
            ..RetryPolicy::default()
        },
    )
    .unwrap();
    let answer = client
        .post_money(
            "/v1/payments",
            &json!({"amount": 4200, "currency": "USD", "token": "tok_sim_approve"}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(creates.load(Ordering::SeqCst), 3);
    assert_eq!(answer.attempts, 3);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body["replayed"], true,
        "the retry replayed the first request"
    );
    let direct = Client::new(
        Config::new(&stack.api, Some(&admin), false).unwrap(),
        RetryPolicy::default(),
    )
    .unwrap();
    let payments = direct.get("/v1/payments").await.unwrap();
    assert_eq!(
        payments.body["payments"].as_array().unwrap().len(),
        1,
        "charged once"
    );
    stack.stop().await;
}

/// A server that accepts connections and never answers.
async fn silent() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    url
}

/// Regression (v1 review L1): an attempt's deadline is capped by what is left of the
/// request's, so `--timeout 5` never waits 10 s for one attempt.
#[tokio::test]
async fn retries_never_overrun_the_overall_deadline() {
    let url = silent().await;
    let policy = RetryPolicy {
        deadline: Duration::from_secs(1),
        attempt_timeout: Duration::from_secs(10),
        ..fast()
    };
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), policy).unwrap();
    let started = std::time::Instant::now();
    let error = client.get("/v1/project").await.unwrap_err();
    assert!(
        matches!(error, ClientError::Unreachable { .. }),
        "{error:?}"
    );
    let read = started.elapsed();
    let started = std::time::Instant::now();
    let error = client
        .post_money("/v1/payments", &json!({}), Some("order-5".to_owned()))
        .await
        .unwrap_err();
    assert!(matches!(error, ClientError::Unknown { .. }), "{error:?}");
    let money = started.elapsed();
    for (what, took) in [("read", read), ("money", money)] {
        assert!(
            took < Duration::from_millis(2_500),
            "{what}: {took:?} for a 1 s deadline"
        );
    }
}

/// Regression (v1 review L2): a plain `POST` (a reset, a key) is re-sent only when no
/// attempt can have been applied: refused connections, `429`, and `503 overloaded` (shed
/// before any handler ran). A `503` from inside the handler (a deadline, the database) may
/// follow a commit, so it is reported as "may or may not have happened", never re-sent.
#[tokio::test]
async fn a_plain_post_is_never_re_sent_after_an_answer_that_may_follow_a_commit() {
    let ok = json!({"reset": {"from_epoch": 1, "to_epoch": 2}, "simulated": true});
    for code in ["deadline_exceeded", "unavailable"] {
        let failed =
            json!({"error": {"code": code, "message": "ran out of time"}, "simulated": true});
        let (url, keys) = fake(vec![(503, failed, true), (200, ok.clone(), true)]).await;
        let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
        let error = client
            .post("/v1/project/reset", &json!({"confirm": "s", "epoch": 1}))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ClientError::Unreachable { cause, .. } if cause.contains("may or may not")),
            "{code}: {error:?}"
        );
        assert_eq!(keys.lock().unwrap().len(), 1, "{code}: re-sent");
    }
    let shed = json!({"error": {"code": "overloaded", "message": "busy"}, "simulated": true});
    let (url, keys) = fake(vec![(503, shed, true), (200, ok, true)]).await;
    let client = Client::new(Config::new(&url, Some(KEY), false).unwrap(), fast()).unwrap();
    let answer = client
        .post("/v1/project/reset", &json!({"confirm": "s", "epoch": 1}))
        .await
        .unwrap();
    assert_eq!((answer.status, answer.attempts), (200, 2));
    assert_eq!(keys.lock().unwrap().len(), 2);
}
