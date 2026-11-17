//! The ship gate's reproducibility (M7, D-069): every library scenario, run twice in one
//! project and once more after a dataset reset under the first run's keys, yields the same
//! states, events and journal entries (ids aside), equal to the trace committed with the
//! library; and the development store, consuming the signed webhooks, fulfills exactly the
//! payments that captured money, once each. SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::time::Duration;

use common::Stack;
use sandbox_cli::scenario::{LIBRARY, RunOptions, Trace, differences, run};
use sandbox_cli::{Client, Config, RetryPolicy};
use serde_json::json;

/// Waits until the store's inbox holds `events` events (deliveries are asynchronous).
async fn delivered(stack: &Stack, events: u64) {
    for _ in 0..300 {
        if stack.store.inbox_len().await.unwrap() >= events {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "the store received {} of {events} events",
        stack.store.inbox_len().await.unwrap()
    );
}

fn events_of(traces: &[Trace]) -> u64 {
    traces
        .iter()
        .flat_map(|t| &t.payments)
        .map(|p| p.events.len() as u64)
        .sum()
}

fn captured_payments(traces: &[Trace]) -> usize {
    traces
        .iter()
        .flat_map(|t| &t.payments)
        .filter(|p| p.captured > 0)
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_library_scenario_runs_twice_and_after_a_reset_with_the_same_trace() {
    let stack = Stack::start().await;
    let (slug, admin, read) = stack.project("scenario-store").await;
    let client = Client::new(
        Config::new(&stack.api, Some(&admin), false).unwrap(),
        RetryPolicy::default(),
    )
    .unwrap();
    let options = RunOptions {
        wait_timeout: Duration::from_secs(30),
    };
    // PS_BLESS_SCENARIOS=1 writes the expected traces from this run (review them before
    // committing: they are the contract every later release is held to).
    let bless = std::env::var_os("PS_BLESS_SCENARIOS").is_some();

    let mut first = Vec::new();
    let mut all = Vec::new();
    for entry in LIBRARY {
        let scenario = entry.scenario().unwrap();
        let a = run(&client, &scenario, "firstrun", options).await.unwrap();
        let b = run(&client, &scenario, "secondrun", options).await.unwrap();
        assert_eq!(
            differences(&a, &b),
            Vec::<String>::new(),
            "{}: two runs differ",
            entry.name
        );
        if bless {
            let path = format!(
                "{}/../../fixtures/scenarios/expected/{}.json",
                env!("CARGO_MANIFEST_DIR"),
                entry.name
            );
            std::fs::write(path, serde_json::to_string_pretty(&a).unwrap() + "\n").unwrap();
        } else {
            let expected = entry.expected_trace().unwrap();
            assert_eq!(
                differences(&expected, &a),
                Vec::<String>::new(),
                "{} no longer produces its expected trace",
                entry.name
            );
        }
        all.push(b);
        first.push(a);
    }
    all.extend(first.iter().cloned());
    delivered(&stack, events_of(&all)).await;

    // A new epoch: the first run's keys again derive new identities (D-015), and every
    // scenario behaves exactly as before.
    let reset = client
        .post("/v1/project/reset", &json!({"confirm": slug, "epoch": 1}))
        .await
        .unwrap();
    assert_eq!(reset.body["project"]["dataset_epoch"], 2);
    for (entry, before) in LIBRARY.iter().zip(&first) {
        let scenario = entry.scenario().unwrap();
        let again = run(&client, &scenario, "firstrun", options).await.unwrap();
        assert_eq!(
            differences(before, &again),
            Vec::<String>::new(),
            "{} after the reset",
            entry.name
        );
        all.push(again);
    }
    delivered(&stack, events_of(&all)).await;

    // The development store fulfilled each payment that captured money, exactly once.
    let fulfillments = stack.store.fulfillments().await.unwrap();
    let distinct: std::collections::BTreeSet<_> =
        fulfillments.iter().map(|f| f.payment_id).collect();
    assert_eq!(
        distinct.len(),
        fulfillments.len(),
        "a payment was fulfilled twice"
    );
    assert_eq!(fulfillments.len(), captured_payments(&all));

    // Every balance still equals its journal, and the network agrees, both ways.
    let reader = Client::new(
        Config::new(&stack.api, Some(&read), false).unwrap(),
        RetryPolicy::default(),
    )
    .unwrap();
    let report = reader.get("/v1/reconciliation?network=true").await.unwrap();
    assert_eq!(report.body["clean"], true, "{}", report.body);
    stack.stop().await;
}
