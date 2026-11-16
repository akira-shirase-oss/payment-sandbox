//! The simulated network's store: one execution per identity, conflicts on reuse, fenced
//! readback that refuses late arrivals, its own capacity checks, durability across a
//! restart, and a virtual clock that never rewrites recorded results.
#![allow(clippy::unwrap_used, reason = "tests: a panic is the failure report")]

use sandbox_domain::network::{ExecuteRequest, Outcome, ReadbackStatus};
use sandbox_domain::{Currency, OperationKind};
use sandbox_sim::store::{Store, StoreError};
use uuid::Uuid;

fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ps-sim-test-{}.redb", Uuid::now_v7().simple()))
}

fn authorize(id: Uuid, amount: i64, token: &str) -> ExecuteRequest {
    ExecuteRequest {
        id,
        project_id: Uuid::from_u128(1),
        kind: OperationKind::Authorize,
        parent_id: None,
        amount_minor: amount,
        currency: Currency::Usd,
        token: Some(token.to_owned()),
    }
}

fn child(id: Uuid, parent: Uuid, kind: OperationKind, amount: i64) -> ExecuteRequest {
    ExecuteRequest {
        id,
        project_id: Uuid::from_u128(1),
        kind,
        parent_id: Some(parent),
        amount_minor: amount,
        currency: Currency::Usd,
        token: None,
    }
}

#[tokio::test]
async fn an_identity_executes_once_and_a_reuse_with_other_fields_conflicts() {
    let store = Store::open(&path()).await.unwrap();
    let id = Uuid::now_v7();
    let first = store
        .execute(&authorize(id, 500, "tok_sim_approve"))
        .await
        .unwrap();
    assert_eq!(first.reply.outcome, Outcome::Approved);
    assert!(!first.reply.replayed);
    let again = store
        .execute(&authorize(id, 500, "tok_sim_approve"))
        .await
        .unwrap();
    assert!(again.reply.replayed);
    assert_eq!(again.reply.network_ref, first.reply.network_ref);
    assert!(matches!(
        store.execute(&authorize(id, 501, "tok_sim_approve")).await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(store.count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_fenced_identity_refuses_every_later_arrival() {
    let store = Store::open(&path()).await.unwrap();
    let id = Uuid::now_v7();
    assert_eq!(
        store.readback(id, false).await.unwrap().status,
        ReadbackStatus::Unknown
    );
    assert_eq!(
        store.readback(id, true).await.unwrap().status,
        ReadbackStatus::NotExecuted
    );
    assert!(matches!(
        store.execute(&authorize(id, 500, "tok_sim_approve")).await,
        Err(StoreError::Fenced)
    ));
    assert_eq!(
        store.readback(id, true).await.unwrap().status,
        ReadbackStatus::NotExecuted
    );
    assert_eq!(store.count().await.unwrap(), 0);
    // A readback of an executed identity returns its record, fence or not.
    let done = Uuid::now_v7();
    store
        .execute(&authorize(done, 5, "tok_sim_approve"))
        .await
        .unwrap();
    let readback = store.readback(done, true).await.unwrap();
    assert_eq!(readback.status, ReadbackStatus::Executed);
    assert_eq!(readback.result.unwrap().outcome, Outcome::Approved);
}

#[tokio::test]
async fn the_network_declines_overruns_on_its_own_record() {
    let store = Store::open(&path()).await.unwrap();
    let auth = Uuid::now_v7();
    store
        .execute(&authorize(auth, 100, "tok_sim_approve"))
        .await
        .unwrap();
    let ok = store
        .execute(&child(Uuid::now_v7(), auth, OperationKind::Capture, 70))
        .await
        .unwrap();
    assert_eq!(ok.reply.outcome, Outcome::Approved);
    let over = store
        .execute(&child(Uuid::now_v7(), auth, OperationKind::Capture, 31))
        .await
        .unwrap();
    assert_eq!(
        (over.reply.outcome, over.reply.decline_code.as_deref()),
        (Outcome::Declined, Some("amount_exceeds_authorization"))
    );
    let refund_over = store
        .execute(&child(Uuid::now_v7(), auth, OperationKind::Refund, 71))
        .await
        .unwrap();
    assert_eq!(
        refund_over.reply.decline_code.as_deref(),
        Some("amount_exceeds_captured")
    );
    let declined = Uuid::now_v7();
    store
        .execute(&authorize(declined, 100, "tok_sim_decline_do_not_honor"))
        .await
        .unwrap();
    let orphan = store
        .execute(&child(Uuid::now_v7(), declined, OperationKind::Capture, 1))
        .await
        .unwrap();
    assert_eq!(
        orphan.reply.decline_code.as_deref(),
        Some("no_authorization")
    );
}

#[tokio::test]
async fn records_survive_a_restart_and_a_clock_jump_never_rewrites_them() {
    let file = path();
    let id = Uuid::now_v7();
    let recorded = {
        let store = Store::open(&file).await.unwrap();
        let executed = store
            .execute(&authorize(id, 500, "tok_sim_approve"))
            .await
            .unwrap();
        store.close().await;
        executed.reply
    };
    let store = Store::open(&file).await.unwrap();
    let before = store.list(0, 100).await.unwrap();
    store.advance(86_400 * 30).await.unwrap();
    store.advance(-86_400 * 60).await.unwrap();
    let after = store.list(0, 100).await.unwrap();
    assert_eq!(
        before, after,
        "the recorded history is unchanged by clock jumps"
    );
    let replay = store
        .execute(&authorize(id, 500, "tok_sim_approve"))
        .await
        .unwrap();
    assert_eq!(replay.reply.virtual_time, recorded.virtual_time);
    assert_eq!(replay.reply.network_ref, recorded.network_ref);
}

#[tokio::test]
async fn listing_pages_through_every_record_in_order() {
    let store = Store::open(&path()).await.unwrap();
    for n in 0..7 {
        store
            .execute(&authorize(Uuid::now_v7(), 10 + n, "tok_sim_approve"))
            .await
            .unwrap();
    }
    let mut after = 0;
    let mut seen = Vec::new();
    loop {
        let page = store.list(after, 3).await.unwrap();
        seen.extend(page.operations.iter().map(|o| o.amount_minor));
        match page.next_after {
            Some(next) => after = next,
            None => break,
        }
    }
    assert_eq!(seen, (10..17).collect::<Vec<_>>());
}
