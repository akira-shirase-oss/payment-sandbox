//! `sandbox-cli` against a sandbox behind HTTPS (the operator guide's shared host, D-071): a
//! TLS-terminating proxy with a private test CA in front of a real sandbox. The CLI speaks TLS,
//! trusts an extra CA only when told to (`--ca-cert`, `PS_CA_CERT`), and says plainly what is
//! wrong when TLS fails, instead of blaming the server. SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use common::Stack;

/// A test CA and a server certificate it signed for `localhost` and `127.0.0.1`, valid for a
/// short window around now (platform verifiers refuse very long-lived server certificates).
struct TestPki {
    ca_pem: String,
    server: rustls::ServerConfig,
}

fn pki() -> TestPki {
    let now = time::OffsetDateTime::now_utc();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name.push(
        rcgen::DnType::CommonName,
        "Payment Sandbox test CA (SIMULATED MONEY ONLY)",
    );
    ca.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    ca.not_before = now - time::Duration::days(1);
    ca.not_after = now + time::Duration::days(30);
    let ca = rcgen::CertifiedIssuer::self_signed(ca, rcgen::KeyPair::generate().unwrap()).unwrap();

    let mut leaf =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .unwrap();
    leaf.distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    leaf.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    leaf.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    leaf.not_before = now - time::Duration::days(1);
    leaf.not_after = now + time::Duration::days(30);
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf_cert = leaf.signed_by(&leaf_key, &ca).unwrap();

    let key = rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der()).unwrap();
    let server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![leaf_cert.der().clone(), ca.der().clone()], key)
        .unwrap();
    TestPki {
        ca_pem: ca.pem(),
        server,
    }
}

/// A TLS-terminating proxy (what Caddy is on the shared host) in front of `upstream`.
async fn tls_proxy(upstream: &str, server: rustls::ServerConfig) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let upstream = upstream.trim_start_matches("http://").to_owned();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, upstream) = (acceptor.clone(), upstream.clone());
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let Ok(mut plain) = tokio::net::TcpStream::connect(&upstream).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut plain).await;
            });
        }
    });
    port
}

async fn cli(api: &str, key: &str, ca: Option<&Path>, args: &[&str]) -> Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_sandbox-cli"));
    command
        .args(args)
        .env_remove("PS_ALLOW_NON_LOCAL")
        .env_remove("PS_CA_CERT")
        .env("PS_API_URL", api)
        .env("PS_API_KEY", key)
        .stdin(std::process::Stdio::null());
    if let Some(ca) = ca {
        command.env("PS_CA_CERT", ca);
    }
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn ca_file(pem: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ps-cli-test-ca-{}.pem",
        uuid::Uuid::now_v7().simple()
    ));
    std::fs::write(&path, pem).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cli_speaks_https_and_says_plainly_when_tls_fails() {
    let stack = Stack::start().await;
    let (_, admin, _) = stack.project("tls-store").await;
    let pki = pki();
    let port = tls_proxy(&stack.api, pki.server).await;
    let ca = ca_file(&pki.ca_pem);
    let https = format!("https://127.0.0.1:{port}");

    // Trusting the test CA: the whole path works over TLS.
    let status = cli(&https, &admin, Some(&ca), &["status"]).await;
    assert_eq!(status.status.code(), Some(0), "{}", stderr(&status));
    assert!(stdout(&status).contains("tls-store"), "{}", stdout(&status));
    let created = cli(
        &format!("https://localhost:{port}"),
        &admin,
        Some(&ca),
        &[
            "payments",
            "create",
            "--amount",
            "42.00",
            "--currency",
            "USD",
        ],
    )
    .await;
    assert_eq!(created.status.code(), Some(0), "{}", stderr(&created));

    // Not trusting it: a TLS failure in plain words, naming what to do; never "is the
    // server running".
    let untrusted = cli(&https, &admin, None, &["status"]).await;
    assert_eq!(untrusted.status.code(), Some(5), "{}", stderr(&untrusted));
    let text = stderr(&untrusted);
    assert!(text.contains("certificate"), "{text}");
    assert!(text.contains("--ca-cert"), "{text}");
    assert!(!text.contains("sandbox-server is running"), "{text}");

    // A money-moving request that failed in the handshake was never sent: no retries, no
    // "unknown outcome", the same plain TLS message.
    let money = cli(
        &https,
        &admin,
        None,
        &[
            "payments",
            "create",
            "--amount",
            "1.00",
            "--currency",
            "USD",
        ],
    )
    .await;
    assert_eq!(money.status.code(), Some(5), "{}", stderr(&money));
    assert!(stderr(&money).contains("certificate"), "{}", stderr(&money));

    // https:// to a plain-HTTP listener, and http:// to the TLS one: each names the fix.
    let plain_port = stack.api.rsplit(':').next().unwrap();
    let mismatch = cli(
        &format!("https://127.0.0.1:{plain_port}"),
        &admin,
        Some(&ca),
        &["status"],
    )
    .await;
    assert_eq!(mismatch.status.code(), Some(5), "{}", stderr(&mismatch));
    assert!(
        stderr(&mismatch).contains("http://"),
        "{}",
        stderr(&mismatch)
    );
    let reverse = cli(
        &format!("http://127.0.0.1:{port}"),
        &admin,
        None,
        &["status"],
    )
    .await;
    assert_eq!(reverse.status.code(), Some(5), "{}", stderr(&reverse));
    assert!(
        stderr(&reverse).contains("https://"),
        "{}",
        stderr(&reverse)
    );

    // A CA file that is not a certificate is a usage error, before anything is sent.
    let bad = ca_file("not a certificate");
    let refused = cli(&https, &admin, Some(&bad), &["status"]).await;
    assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("PEM"), "{}", stderr(&refused));

    for file in [ca, bad] {
        let _ = std::fs::remove_file(file);
    }
    stack.stop().await;
}
