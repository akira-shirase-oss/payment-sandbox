//! Review finding 10: the development store's `PUT /config` swaps the API key and the
//! signing secrets it trusts, so it is not open to whoever can reach the port. It exists
//! only when the startup flag turns it on (the `dev` profile's default) and then needs the
//! operator's credential. The real binary is started the way an operator would start it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use uuid::Uuid;

const CONTROL: &str = "store-control-credential-0123456789abc";

struct Store {
    child: Child,
    base: String,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(vars: &[(&str, &str)]) -> (Command, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let db = std::env::temp_dir().join(format!("ps-store-ctl-{}.redb", Uuid::now_v7().simple()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_sandbox-dev-store"));
    command
        .env_clear()
        .env("PS_STORE_LISTEN", format!("127.0.0.1:{port}"))
        .env("PS_STORE_DB", db)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in vars {
        command.env(k, v);
    }
    (command, format!("http://127.0.0.1:{port}"))
}

async fn start(vars: &[(&str, &str)]) -> Store {
    let (mut command, base) = command(vars);
    let store = Store {
        child: command.spawn().unwrap(),
        base,
    };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(r) = http.get(format!("{}/healthz", store.base)).send().await
            && r.status().is_success()
        {
            return store;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the store never answered /healthz");
}

fn config() -> serde_json::Value {
    json!({
        "sandbox_url": "http://127.0.0.1:9",
        "api_key": "sk_sim_attacker",
        "secrets": ["whsec_attacker"],
    })
}

async fn put(store: &Store, bearer: Option<&str>) -> u16 {
    let mut request = reqwest::Client::new()
        .put(format!("{}/config", store.base))
        .json(&config());
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn config_is_absent_outside_dev_and_needs_the_operator_credential_in_dev() {
    let shared = start(&[]).await;
    assert_eq!(put(&shared, None).await, 404);
    assert_eq!(put(&shared, Some(CONTROL)).await, 404);

    let dev = start(&[("PS_PROFILE", "dev"), ("PS_STORE_CONTROL_TOKEN", CONTROL)]).await;
    assert_eq!(put(&dev, None).await, 401);
    assert_eq!(
        put(&dev, Some("not-the-credential-0123456789abcdef")).await,
        401
    );
    assert_eq!(put(&dev, Some(CONTROL)).await, 200);
}

#[test]
fn it_refuses_a_control_plane_without_a_credential() {
    for vars in [
        &[("PS_PROFILE", "dev")][..],
        &[("PS_STORE_CONTROL", "on")][..],
        &[("PS_PROFILE", "staging")][..],
    ] {
        let (mut command, _) = command(vars);
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut refused = false;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                refused = !status.success();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(refused, "started with {vars:?}");
    }
}
