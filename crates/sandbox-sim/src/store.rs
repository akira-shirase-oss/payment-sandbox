//! The simulated network's store (D-018, D-034): one redb file owned by one writer thread.
//!
//! redb is a pure-Rust, ACID, crash-safe embedded database; commits are durable (fsync)
//! before they return. Requests reach the writer through a **bounded** queue (`QUEUE`,
//! 4,096); a full queue is [`StoreError::Overloaded`], never an unbounded backlog. The
//! writer takes up to `BATCH` queued commands and runs them in **one** write transaction
//! (group commit), answering only after that commit is durable. A command that is refused
//! (conflict, fenced, invalid) writes nothing, so it cannot affect the others in its batch.
//! Executions are serialized, so "exists?" and "insert" never race.
//!
//! Records are immutable: there is no code path that updates or deletes an operation.

use std::path::{Path, PathBuf};
use std::thread;

use redb::{Database, ReadableTable, ReadableTableMetadata, Table, TableDefinition};
use sandbox_domain::network::{
    ExecuteReply, ExecuteRequest, ListedOperation, OperationPage, Outcome, Readback,
    ReadbackStatus, network_ref,
};
use sandbox_domain::scenario::Scenario;
use sandbox_domain::{Currency, OperationKind};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Bound of the command queue.
pub const QUEUE: usize = 4_096;
/// Most commands in one transaction.
pub const BATCH: usize = 128;
/// The virtual clock's starting point (2026-01-01T00:00:00Z).
pub const CLOCK_EPOCH: i64 = 1_767_225_600;

const OPERATIONS: TableDefinition<'_, u128, &[u8]> = TableDefinition::new("operations");
const BY_SEQ: TableDefinition<'_, u64, u128> = TableDefinition::new("operations_by_seq");
const BY_PROJECT: TableDefinition<'_, (u128, u64), u128> =
    TableDefinition::new("operations_by_project");
const TOMBSTONES: TableDefinition<'_, u128, i64> = TableDefinition::new("tombstones");
const AUTHORIZATIONS: TableDefinition<'_, u128, &[u8]> = TableDefinition::new("authorizations");
const META: TableDefinition<'_, &str, i64> = TableDefinition::new("meta");

/// Store failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The same id arrived with different fields.
    #[error("operation id reused with a different request")]
    Conflict,
    /// The id was fenced by a readback; a late arrival is refused (D-017).
    #[error("operation id fenced")]
    Fenced,
    /// The request is not executable (unknown scenario token, missing token).
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    /// The queue is full: shed, nothing executed.
    #[error("the network's store is overloaded")]
    Overloaded,
    /// The writer thread has stopped.
    #[error("the network's store is closed")]
    Closed,
    /// The database failed.
    #[error("store error: {0}")]
    Storage(String),
}

fn storage(e: impl std::fmt::Display) -> StoreError {
    StoreError::Storage(e.to_string())
}

/// How an execution went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executed {
    /// The recorded (or replayed) answer.
    pub reply: ExecuteReply,
}

/// One recorded operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    seq: u64,
    project_id: Uuid,
    kind: OperationKind,
    parent_id: Option<Uuid>,
    amount_minor: i64,
    currency: Currency,
    token: Option<String>,
    fingerprint: String,
    outcome: Outcome,
    decline_code: Option<String>,
    network_ref: String,
    virtual_time: i64,
}

/// The network's own running record of an approved authorization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Authorization {
    authorized: i64,
    currency: Currency,
    captured: i64,
    voided: i64,
    refunded: i64,
}

enum Command {
    Execute(
        Box<ExecuteRequest>,
        oneshot::Sender<Result<Executed, StoreError>>,
    ),
    Readback(Uuid, bool, oneshot::Sender<Result<Readback, StoreError>>),
    List(
        Option<Uuid>,
        i64,
        i64,
        oneshot::Sender<Result<OperationPage, StoreError>>,
    ),
    Advance(i64, oneshot::Sender<Result<i64, StoreError>>),
    Now(oneshot::Sender<Result<i64, StoreError>>),
    Count(oneshot::Sender<Result<i64, StoreError>>),
    Close(oneshot::Sender<()>),
}

/// The network's store: a handle to the writer thread.
#[derive(Debug, Clone)]
pub struct Store {
    commands: mpsc::Sender<Command>,
    path: PathBuf,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Command")
    }
}

impl Store {
    /// Opens (creating if needed) the store at `path` and starts its writer.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let path = path.to_path_buf();
        let (commands, receiver) = mpsc::channel(QUEUE);
        let (ready_tx, ready_rx) = oneshot::channel();
        let thread_path = path.clone();
        thread::Builder::new()
            .name("sim-store".to_owned())
            .spawn(move || match open_database(&thread_path) {
                Ok(db) => {
                    let _ = ready_tx.send(Ok(()));
                    writer(db, receiver);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(storage)?;
        ready_rx.await.map_err(|_| StoreError::Closed)??;
        Ok(Self { commands, path })
    }

    /// The store's file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops the writer after what is already queued (a clean process stop): the file is
    /// closed, and every later command fails with [`StoreError::Closed`].
    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Command::Close(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> Command,
    ) -> Result<T, StoreError> {
        let (tx, rx) = oneshot::channel();
        self.commands.try_send(make(tx)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => StoreError::Overloaded,
            mpsc::error::TrySendError::Closed(_) => StoreError::Closed,
        })?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    /// Executes `request` once under its identity, or replays the recorded result.
    pub async fn execute(&self, request: &ExecuteRequest) -> Result<Executed, StoreError> {
        let request = Box::new(request.clone());
        self.ask(|tx| Command::Execute(request, tx)).await
    }

    /// Reads back what the network recorded for `id`. With `fence`, an unknown id is
    /// tombstoned in the same transaction, so a delayed arrival can never execute later.
    pub async fn readback(&self, id: Uuid, fence: bool) -> Result<Readback, StoreError> {
        self.ask(|tx| Command::Readback(id, fence, tx)).await
    }

    /// Lists recorded operations after `after` (allocation order), at most `limit`, of
    /// one project when `project` is given.
    pub async fn list(&self, after: i64, limit: i64) -> Result<OperationPage, StoreError> {
        self.ask(|tx| Command::List(None, after, limit, tx)).await
    }

    /// Lists one project's recorded operations after `after`, at most `limit`.
    pub async fn list_project(
        &self,
        project: Uuid,
        after: i64,
        limit: i64,
    ) -> Result<OperationPage, StoreError> {
        self.ask(|tx| Command::List(Some(project), after, limit, tx))
            .await
    }

    /// The virtual clock, in Unix seconds.
    pub async fn now(&self) -> Result<i64, StoreError> {
        self.ask(Command::Now).await
    }

    /// Moves the virtual clock by `seconds` (either direction). Recorded results keep the
    /// virtual time they were executed at: the clock drives scenarios, never history (D-008).
    pub async fn advance(&self, seconds: i64) -> Result<i64, StoreError> {
        self.ask(|tx| Command::Advance(seconds, tx)).await
    }

    /// Number of recorded operations (for drills and tests).
    pub async fn count(&self) -> Result<i64, StoreError> {
        self.ask(Command::Count).await
    }
}

fn open_database(path: &Path) -> Result<Database, StoreError> {
    let db = Database::create(path).map_err(storage)?;
    let txn = db.begin_write().map_err(storage)?;
    {
        // Create every table, so readers never meet a missing one.
        txn.open_table(OPERATIONS).map_err(storage)?;
        txn.open_table(BY_SEQ).map_err(storage)?;
        txn.open_table(BY_PROJECT).map_err(storage)?;
        txn.open_table(TOMBSTONES).map_err(storage)?;
        txn.open_table(AUTHORIZATIONS).map_err(storage)?;
        let mut meta = txn.open_table(META).map_err(storage)?;
        if meta.get("clock").map_err(storage)?.is_none() {
            meta.insert("clock", CLOCK_EPOCH).map_err(storage)?;
        }
    }
    txn.commit().map_err(storage)?;
    Ok(db)
}

type Reply = Box<dyn FnOnce() + Send>;

/// The writer loop: drain up to `BATCH` commands, run them in one transaction, answer
/// after the durable commit.
fn writer(db: Database, mut receiver: mpsc::Receiver<Command>) {
    let mut batch: Vec<Command> = Vec::with_capacity(BATCH);
    while let Some(first) = receiver.blocking_recv() {
        batch.push(first);
        while batch.len() < BATCH {
            match receiver.try_recv() {
                Ok(command) => batch.push(command),
                Err(_) => break,
            }
        }
        let mut closing = None;
        if let Some(index) = batch.iter().position(|c| matches!(c, Command::Close(_))) {
            // Run what came before the close; what came after it is refused (dropped
            // senders answer `Closed`).
            batch.truncate(index.saturating_add(1));
            if let Some(Command::Close(done)) = batch.pop() {
                closing = Some(done);
            }
            receiver.close();
        }
        let mut replies: Vec<Reply> = Vec::with_capacity(batch.len());
        if let Err(e) = run_batch(&db, batch.drain(..), &mut replies) {
            tracing::error!(error = %e, "the network's store failed to commit a batch");
            // Nothing in the batch is durable: every caller learns it failed (an honest
            // "unknown" for the server, which reads back later).
            replies.clear();
        }
        for reply in replies {
            reply();
        }
        if let Some(done) = closing {
            drop(db);
            let _ = done.send(());
            return;
        }
    }
}

struct Tables<'t> {
    operations: Table<'t, u128, &'static [u8]>,
    by_seq: Table<'t, u64, u128>,
    by_project: Table<'t, (u128, u64), u128>,
    tombstones: Table<'t, u128, i64>,
    authorizations: Table<'t, u128, &'static [u8]>,
    meta: Table<'t, &'static str, i64>,
}

fn run_batch(
    db: &Database,
    commands: std::vec::Drain<'_, Command>,
    replies: &mut Vec<Reply>,
) -> Result<(), StoreError> {
    let txn = db.begin_write().map_err(storage)?;
    {
        let mut t = Tables {
            operations: txn.open_table(OPERATIONS).map_err(storage)?,
            by_seq: txn.open_table(BY_SEQ).map_err(storage)?,
            by_project: txn.open_table(BY_PROJECT).map_err(storage)?,
            tombstones: txn.open_table(TOMBSTONES).map_err(storage)?,
            authorizations: txn.open_table(AUTHORIZATIONS).map_err(storage)?,
            meta: txn.open_table(META).map_err(storage)?,
        };
        for command in commands {
            match command {
                Command::Execute(request, sender) => {
                    let result = execute(&mut t, &request);
                    replies.push(Box::new(move || drop(sender.send(result))));
                }
                Command::Readback(id, fence, sender) => {
                    let result = readback(&mut t, id, fence);
                    replies.push(Box::new(move || drop(sender.send(result))));
                }
                Command::List(project, after, limit, sender) => {
                    let result = list(&t, project, after, limit);
                    replies.push(Box::new(move || drop(sender.send(result))));
                }
                Command::Advance(seconds, sender) => {
                    let result = advance(&mut t, seconds);
                    replies.push(Box::new(move || drop(sender.send(result))));
                }
                Command::Now(sender) => {
                    let result = now(&t);
                    replies.push(Box::new(move || drop(sender.send(result))));
                }
                Command::Count(sender) => {
                    let result = t
                        .operations
                        .len()
                        .map_err(storage)
                        .map(|n| i64::try_from(n).unwrap_or(i64::MAX));
                    replies.push(Box::new(move || drop(sender.send(result))));
                }
                Command::Close(done) => drop(done),
            }
        }
    }
    txn.commit().map_err(storage)?;
    Ok(())
}

fn now(t: &Tables<'_>) -> Result<i64, StoreError> {
    Ok(t.meta
        .get("clock")
        .map_err(storage)?
        .map_or(CLOCK_EPOCH, |g| g.value()))
}

fn advance(t: &mut Tables<'_>, seconds: i64) -> Result<i64, StoreError> {
    let next = now(t)?.saturating_add(seconds);
    t.meta.insert("clock", next).map_err(storage)?;
    Ok(next)
}

fn next_seq(t: &mut Tables<'_>) -> Result<u64, StoreError> {
    let current = t
        .meta
        .get("next_seq")
        .map_err(storage)?
        .map_or(1, |g| g.value());
    t.meta
        .insert("next_seq", current.saturating_add(1))
        .map_err(storage)?;
    u64::try_from(current).map_err(|_| StoreError::Invalid("sequence"))
}

fn load<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, StoreError> {
    serde_json::from_slice(bytes).map_err(storage)
}

fn get_stored(t: &Tables<'_>, id: Uuid) -> Result<Option<Stored>, StoreError> {
    match t.operations.get(id.as_u128()).map_err(storage)? {
        Some(guard) => Ok(Some(load(guard.value())?)),
        None => Ok(None),
    }
}

fn reply_of(id: Uuid, stored: &Stored, replayed: bool) -> ExecuteReply {
    ExecuteReply {
        id,
        outcome: stored.outcome,
        decline_code: stored.decline_code.clone(),
        network_ref: stored.network_ref.clone(),
        virtual_time: stored.virtual_time,
        replayed,
        fingerprint: stored.fingerprint.clone(),
        kind: stored.kind,
        amount_minor: stored.amount_minor,
        simulated: true,
    }
}

fn execute(t: &mut Tables<'_>, request: &ExecuteRequest) -> Result<Executed, StoreError> {
    let key = request.id.as_u128();
    if t.tombstones.get(key).map_err(storage)?.is_some() {
        return Err(StoreError::Fenced);
    }
    let fingerprint = request.fingerprint();
    if let Some(stored) = get_stored(t, request.id)? {
        if stored.fingerprint != fingerprint {
            return Err(StoreError::Conflict);
        }
        return Ok(Executed {
            reply: reply_of(request.id, &stored, true),
        });
    }
    let (outcome, decline, parent) = decide(t, request)?;
    let stored = Stored {
        seq: next_seq(t)?,
        project_id: request.project_id,
        kind: request.kind,
        parent_id: request.parent_id,
        amount_minor: request.amount_minor,
        currency: request.currency,
        token: request.token.clone(),
        fingerprint,
        outcome,
        decline_code: decline.map(str::to_owned),
        network_ref: network_ref(request.id),
        virtual_time: now(t)?,
    };
    let bytes = serde_json::to_vec(&stored).map_err(storage)?;
    t.operations
        .insert(key, bytes.as_slice())
        .map_err(storage)?;
    t.by_seq.insert(stored.seq, key).map_err(storage)?;
    t.by_project
        .insert((request.project_id.as_u128(), stored.seq), key)
        .map_err(storage)?;
    if outcome == Outcome::Approved {
        match (request.kind, parent) {
            (OperationKind::Authorize, _) => {
                let auth = Authorization {
                    authorized: request.amount_minor,
                    currency: request.currency,
                    captured: 0,
                    voided: 0,
                    refunded: 0,
                };
                save_authorization(t, request.id, &auth)?;
            }
            (kind, Some((parent_id, mut auth))) => {
                let amount = request.amount_minor;
                match kind {
                    OperationKind::Capture => auth.captured = auth.captured.saturating_add(amount),
                    OperationKind::Void => auth.voided = auth.voided.saturating_add(amount),
                    OperationKind::Refund => auth.refunded = auth.refunded.saturating_add(amount),
                    OperationKind::Authorize => {}
                }
                save_authorization(t, parent_id, &auth)?;
            }
            (_, None) => {}
        }
    }
    Ok(Executed {
        reply: reply_of(request.id, &stored, false),
    })
}

fn save_authorization(
    t: &mut Tables<'_>,
    id: Uuid,
    auth: &Authorization,
) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec(auth).map_err(storage)?;
    t.authorizations
        .insert(id.as_u128(), bytes.as_slice())
        .map_err(storage)?;
    Ok(())
}

type Decision = (Outcome, Option<&'static str>, Option<(Uuid, Authorization)>);

/// The network's decision. Authorizations follow their scenario token; captures, voids and
/// refunds are checked against the network's *own* record of the authorization, so a server
/// that tried to overrun capacity would be declined here too.
fn decide(t: &Tables<'_>, request: &ExecuteRequest) -> Result<Decision, StoreError> {
    if request.kind == OperationKind::Authorize {
        let token = request
            .token
            .as_deref()
            .ok_or(StoreError::Invalid("authorize needs a token"))?;
        let scenario = Scenario::from_token(token)
            .map_err(|_| StoreError::Invalid("unknown scenario token"))?;
        return Ok(match scenario.decline() {
            Some(code) => (Outcome::Declined, Some(code.code()), None),
            None => (Outcome::Approved, None, None),
        });
    }
    let Some(parent) = request.parent_id else {
        return Ok((Outcome::Declined, Some("no_authorization"), None));
    };
    let auth: Authorization = match t.authorizations.get(parent.as_u128()).map_err(storage)? {
        Some(guard) => load(guard.value())?,
        None => return Ok((Outcome::Declined, Some("no_authorization"), None)),
    };
    if auth.currency != request.currency {
        return Ok((Outcome::Declined, Some("currency_mismatch"), None));
    }
    let open = auth
        .authorized
        .saturating_sub(auth.captured)
        .saturating_sub(auth.voided);
    let refundable = auth.captured.saturating_sub(auth.refunded);
    let amount = request.amount_minor;
    Ok(match request.kind {
        OperationKind::Capture | OperationKind::Void if amount > open => (
            Outcome::Declined,
            Some("amount_exceeds_authorization"),
            None,
        ),
        OperationKind::Refund if amount > refundable => {
            (Outcome::Declined, Some("amount_exceeds_captured"), None)
        }
        _ => (Outcome::Approved, None, Some((parent, auth))),
    })
}

fn readback(t: &mut Tables<'_>, id: Uuid, fence: bool) -> Result<Readback, StoreError> {
    if let Some(stored) = get_stored(t, id)? {
        return Ok(Readback {
            id,
            status: ReadbackStatus::Executed,
            result: Some(reply_of(id, &stored, true)),
        });
    }
    if t.tombstones.get(id.as_u128()).map_err(storage)?.is_some() {
        return Ok(Readback {
            id,
            status: ReadbackStatus::NotExecuted,
            result: None,
        });
    }
    if !fence {
        return Ok(Readback {
            id,
            status: ReadbackStatus::Unknown,
            result: None,
        });
    }
    let at = now(t)?;
    t.tombstones.insert(id.as_u128(), at).map_err(storage)?;
    Ok(Readback {
        id,
        status: ReadbackStatus::NotExecuted,
        result: None,
    })
}

fn list(
    t: &Tables<'_>,
    project: Option<Uuid>,
    after: i64,
    limit: i64,
) -> Result<OperationPage, StoreError> {
    let limit = usize::try_from(limit.clamp(1, 1_000)).unwrap_or(1_000);
    let start = u64::try_from(after.max(0)).unwrap_or(0).saturating_add(1);
    let mut keys: Vec<(u64, u128)> = Vec::with_capacity(limit);
    match project {
        Some(project) => {
            let p = project.as_u128();
            for entry in t
                .by_project
                .range((p, start)..=(p, u64::MAX))
                .map_err(storage)?
            {
                if keys.len() == limit {
                    break;
                }
                let (k, v) = entry.map_err(storage)?;
                keys.push((k.value().1, v.value()));
            }
        }
        None => {
            for entry in t.by_seq.range(start..).map_err(storage)? {
                if keys.len() == limit {
                    break;
                }
                let (k, v) = entry.map_err(storage)?;
                keys.push((k.value(), v.value()));
            }
        }
    }
    let mut operations = Vec::with_capacity(keys.len());
    for (seq, key) in keys {
        let id = Uuid::from_u128(key);
        let stored = get_stored(t, id)?.ok_or(StoreError::Invalid("index without record"))?;
        operations.push(ListedOperation {
            seq: i64::try_from(seq).unwrap_or(i64::MAX),
            id,
            project_id: stored.project_id,
            kind: stored.kind,
            parent_id: stored.parent_id,
            amount_minor: stored.amount_minor,
            currency: stored.currency,
            outcome: stored.outcome,
            virtual_time: stored.virtual_time,
        });
    }
    let next_after = if operations.len() == limit {
        operations.last().map(|o| o.seq)
    } else {
        None
    };
    Ok(OperationPage {
        operations,
        next_after,
    })
}
