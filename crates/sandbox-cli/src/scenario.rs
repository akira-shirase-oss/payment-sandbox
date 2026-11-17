//! The seeded scenario library (D-069): payment-failure stories a development store can
//! replay against the sandbox, each with the trace it must produce.
//!
//! A scenario is data (`fixtures/scenarios/<name>.json`): numbered steps that authorize,
//! capture, void, refund, wait for the reconciler, or repeat an earlier step. Running it
//! gives a [`Trace`]: what every step answered and, for every payment, its operations, its
//! events in sequence and its journal entries — ids, keys and times aside, so two runs
//! compare equal exactly when the sandbox behaved the same. The expected trace of each
//! library scenario is committed next to it (`fixtures/scenarios/expected/<name>.json`) and
//! compiled into the CLI, so a release that changes any of them is caught by the tests and
//! by `sandbox-cli scenarios run --all --check` on any developer's machine.
//!
//! Each run uses fresh idempotency keys derived from its run id, so running a scenario twice
//! in one project gives two independent sets of payments with the same trace.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{Answer, Client, ClientError};

/// A scenario: a name, one plain sentence, and its steps.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// `[a-z0-9-]`, the file's name.
    pub name: String,
    /// What it shows, in one sentence.
    pub summary: String,
    /// The steps, numbered from 1 in traces.
    pub steps: Vec<Step>,
}

/// One step. Amounts are minor units of the payment's currency.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// `POST /v1/payments` with the scenario token `tok_sim_<scenario>`.
    Authorize {
        /// The payment's name within the scenario.
        payment: String,
        /// Minor units.
        amount: i64,
        /// `USD`, `EUR`, `JPY` or `KWD`.
        currency: String,
        /// What the simulated network does: `approve`, `decline_insufficient_funds`,
        /// `decline_do_not_honor`, `decline_expired`, `lost_reply`, `unavailable`.
        scenario: String,
    },
    /// `POST /v1/payments/{id}/captures`.
    Capture {
        /// The payment's name.
        payment: String,
        /// Minor units.
        amount: i64,
    },
    /// `POST /v1/payments/{id}/void`.
    Void {
        /// The payment's name.
        payment: String,
    },
    /// `POST /v1/payments/{id}/refunds`.
    Refund {
        /// The payment's name.
        payment: String,
        /// Minor units.
        amount: i64,
    },
    /// Waits until no operation of the payment is pending or uncertain (the reconciler
    /// reads the network's record back, D-017), or the wait's deadline passes.
    Wait {
        /// The payment's name.
        payment: String,
    },
    /// Sends an earlier money-moving step again under the same idempotency key, optionally
    /// with another amount (which must be a conflict, never a reinterpretation, D-016).
    Repeat {
        /// The earlier step's number (from 1).
        step: usize,
        /// Another amount, to show the conflict.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<i64>,
    },
}

/// An operation as a trace shows it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationTrace {
    /// `authorize`, `capture`, `void` or `refund`.
    pub kind: String,
    /// `pending`, `uncertain`, `succeeded`, `declined` or `not_executed`.
    pub state: String,
    /// Minor units.
    pub amount: i64,
    /// The network's decline code, if declined.
    pub decline_code: Option<String>,
}

/// What one step answered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepTrace {
    /// Its number, from 1.
    pub step: usize,
    /// `authorize`, `capture`, `void`, `refund`, `wait` or `repeat`.
    pub action: String,
    /// The payment's name.
    pub payment: String,
    /// The HTTP status (not for `wait`).
    pub http: Option<u16>,
    /// The operation's state, `refused:<code>` for an API refusal, or for `wait`
    /// `settled` / `still_open`.
    pub result: String,
    /// True when the server replayed a stored answer.
    pub replayed: Option<bool>,
    /// The payment's status after the step.
    pub payment_status: Option<String>,
    /// The step's operation.
    pub operation: Option<OperationTrace>,
}

/// An event as a trace shows it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventTrace {
    /// `<kind>.<outcome>`.
    pub kind: String,
    /// Its per-payment sequence number.
    pub sequence: i64,
}

/// A journal entry as a trace shows it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EntryTrace {
    /// The posting recipe.
    pub recipe: String,
    /// `(account, signed minor units)` in line order.
    pub lines: Vec<(String, i64)>,
}

/// A payment at the end of the run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentTrace {
    /// Its name within the scenario.
    pub payment: String,
    /// Its status.
    pub status: String,
    /// Currency.
    pub currency: String,
    /// Amount authorized for.
    pub amount: i64,
    /// Sums, minor units.
    pub authorized: i64,
    /// Captured.
    pub captured: i64,
    /// Voided.
    pub voided: i64,
    /// Refunded.
    pub refunded: i64,
    /// Its operations, oldest first.
    pub operations: Vec<OperationTrace>,
    /// Its events, by sequence.
    pub events: Vec<EventTrace>,
    /// Its journal entries, in posting order.
    pub journal: Vec<EntryTrace>,
}

/// A run's trace: equal for two runs exactly when the sandbox behaved the same.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    /// The scenario.
    pub scenario: String,
    /// Every step, in order.
    pub steps: Vec<StepTrace>,
    /// Every payment, in order of first mention.
    pub payments: Vec<PaymentTrace>,
}

/// Why a run stopped before producing a trace.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// The scenario itself is wrong.
    #[error("scenario {scenario}, step {step}: {problem}")]
    Invalid {
        /// The scenario.
        scenario: String,
        /// The step (0 for the whole scenario).
        step: usize,
        /// What is wrong.
        problem: String,
    },
    /// The sandbox could not be asked, or a step's outcome is unknown.
    #[error("scenario {scenario}, step {step}: {source}")]
    Client {
        /// The scenario.
        scenario: String,
        /// The step.
        step: usize,
        /// The failure.
        source: ClientError,
    },
}

/// How a run behaves.
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// Longest a `wait` step waits.
    pub wait_timeout: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            wait_timeout: Duration::from_secs(30),
        }
    }
}

/// A library scenario: its definition and its expected trace, as JSON text.
#[derive(Debug, Clone, Copy)]
pub struct LibraryEntry {
    /// Its name.
    pub name: &'static str,
    /// `fixtures/scenarios/<name>.json`.
    pub definition: &'static str,
    /// `fixtures/scenarios/expected/<name>.json`.
    pub expected: &'static str,
}

macro_rules! library {
    ($($name:literal),* $(,)?) => {
        &[$(LibraryEntry {
            name: $name,
            definition: include_str!(concat!("../../../fixtures/scenarios/", $name, ".json")),
            expected: include_str!(concat!("../../../fixtures/scenarios/expected/", $name, ".json")),
        }),*]
    };
}

/// The seeded library, in the order `scenarios list` shows it. Preserved across releases:
/// removing or changing one is a breaking change the tests report.
pub const LIBRARY: &[LibraryEntry] = library!(
    "purchase",
    "decline-insufficient-funds",
    "decline-then-approve",
    "expired-payment-method",
    "lost-reply",
    "network-unavailable",
    "partial-capture-then-void",
    "refund-capacity",
    "idempotent-retry",
    "capture-over-authorization",
    "currencies",
);

impl LibraryEntry {
    /// The parsed definition.
    pub fn scenario(&self) -> Result<Scenario, RunError> {
        parse(self.definition)
    }

    /// The parsed expected trace.
    pub fn expected_trace(&self) -> Result<Trace, RunError> {
        serde_json::from_str(self.expected).map_err(|e| RunError::Invalid {
            scenario: self.name.to_owned(),
            step: 0,
            problem: format!("its expected trace does not parse: {e}"),
        })
    }
}

/// The library scenario called `name`.
pub fn find(name: &str) -> Option<&'static LibraryEntry> {
    LIBRARY.iter().find(|entry| entry.name == name)
}

/// Parses and checks a scenario definition.
pub fn parse(text: &str) -> Result<Scenario, RunError> {
    let scenario: Scenario = serde_json::from_str(text).map_err(|e| RunError::Invalid {
        scenario: "?".to_owned(),
        step: 0,
        problem: format!("not a scenario: {e}"),
    })?;
    validate(&scenario)?;
    Ok(scenario)
}

fn validate(scenario: &Scenario) -> Result<(), RunError> {
    let invalid = |step: usize, problem: String| RunError::Invalid {
        scenario: scenario.name.clone(),
        step,
        problem,
    };
    if sandbox_domain::project::ProjectSlug::parse(&scenario.name).is_err() {
        return Err(invalid(
            0,
            "the name must be 1-63 of a-z, 0-9 and -".to_owned(),
        ));
    }
    if scenario.steps.is_empty() || scenario.steps.len() > 50 {
        return Err(invalid(0, "a scenario has 1 to 50 steps".to_owned()));
    }
    let mut known: Vec<&str> = Vec::new();
    for (index, step) in scenario.steps.iter().enumerate() {
        let number = index + 1;
        match step {
            Step::Authorize {
                payment,
                currency,
                scenario: token,
                ..
            } => {
                sandbox_domain::Currency::from_code(currency)
                    .map_err(|e| invalid(number, e.to_string()))?;
                sandbox_domain::scenario::Scenario::from_token(&format!("tok_sim_{token}"))
                    .map_err(|e| invalid(number, e.to_string()))?;
                if known.contains(&payment.as_str()) {
                    return Err(invalid(
                        number,
                        format!("payment {payment} is authorized twice"),
                    ));
                }
                known.push(payment);
            }
            Step::Capture { payment, .. }
            | Step::Void { payment }
            | Step::Refund { payment, .. }
            | Step::Wait { payment } => {
                if !known.contains(&payment.as_str()) {
                    return Err(invalid(
                        number,
                        format!("payment {payment} is not authorized before this step"),
                    ));
                }
            }
            Step::Repeat { step, .. } => {
                let earlier = step
                    .checked_sub(1)
                    .and_then(|i| scenario.steps.get(i))
                    .filter(|_| *step < number);
                if !matches!(
                    earlier,
                    Some(
                        Step::Authorize { .. }
                            | Step::Capture { .. }
                            | Step::Void { .. }
                            | Step::Refund { .. }
                    )
                ) {
                    return Err(invalid(
                        number,
                        format!("step {step} is not an earlier money-moving step"),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A run id: letters only, so keys built from it carry no digits but step numbers.
pub fn valid_run_id(run_id: &str) -> bool {
    (1..=40).contains(&run_id.len()) && run_id.bytes().all(|b| b.is_ascii_lowercase())
}

/// A fresh run id: 16 letters `a`–`p`.
pub fn new_run_id() -> String {
    let key = crate::client::new_idempotency_key();
    key.trim_start_matches("cli_")[..16].to_owned()
}

struct Sent {
    path: String,
    body: Value,
    key: String,
    payment: String,
    action: &'static str,
}

/// What a step does: send a money-moving request, repeat one, or wait.
enum Plan<'a> {
    Send(Sent),
    Repeat { step: usize, amount: Option<i64> },
    Wait(&'a str),
}

/// The request a step sends, with the payment ids known so far.
fn plan<'a>(
    scenario: &Scenario,
    number: usize,
    step: &'a Step,
    key: &str,
    ids: &BTreeMap<String, String>,
) -> Result<Plan<'a>, RunError> {
    let id_of = |payment: &str| {
        ids.get(payment).cloned().ok_or_else(|| RunError::Invalid {
            scenario: scenario.name.clone(),
            step: number,
            problem: format!("payment {payment} was refused, so this step has nothing to act on"),
        })
    };
    let send = |path: String, body: Value, payment: &str, action| {
        Plan::Send(Sent {
            path,
            body,
            key: key.to_owned(),
            payment: payment.to_owned(),
            action,
        })
    };
    Ok(match step {
        Step::Authorize {
            payment,
            amount,
            currency,
            scenario: token,
        } => send(
            "/v1/payments".to_owned(),
            json!({"amount": amount, "currency": currency, "token": format!("tok_sim_{token}")}),
            payment,
            "authorize",
        ),
        Step::Capture { payment, amount } => send(
            format!("/v1/payments/{}/captures", id_of(payment)?),
            json!({"amount": amount}),
            payment,
            "capture",
        ),
        Step::Void { payment } => send(
            format!("/v1/payments/{}/void", id_of(payment)?),
            json!({}),
            payment,
            "void",
        ),
        Step::Refund { payment, amount } => send(
            format!("/v1/payments/{}/refunds", id_of(payment)?),
            json!({"amount": amount}),
            payment,
            "refund",
        ),
        Step::Repeat { step, amount } => Plan::Repeat {
            step: *step,
            amount: *amount,
        },
        Step::Wait { payment } => Plan::Wait(payment),
    })
}

/// Runs `scenario` through `client` and returns its trace. Every money-moving step's key is
/// `scn_<run id>_<scenario>_<step>`, so the same run id replays (the server answers from
/// its records) and a new one starts afresh.
pub async fn run(
    client: &Client,
    scenario: &Scenario,
    run_id: &str,
    options: RunOptions,
) -> Result<Trace, RunError> {
    validate(scenario)?;
    if !valid_run_id(run_id) {
        return Err(RunError::Invalid {
            scenario: scenario.name.clone(),
            step: 0,
            problem: "a run id is 1-40 lowercase letters".to_owned(),
        });
    }
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut sent: BTreeMap<usize, Sent> = BTreeMap::new();
    let mut steps = Vec::new();
    for (index, step) in scenario.steps.iter().enumerate() {
        let number = index + 1;
        let fail = |source: ClientError| RunError::Client {
            scenario: scenario.name.clone(),
            step: number,
            source,
        };
        let key = format!("scn_{run_id}_{}_{number}", scenario.name);
        match plan(scenario, number, step, &key, &ids)? {
            Plan::Send(request) => {
                let answer = client
                    .post_money(&request.path, &request.body, Some(request.key.clone()))
                    .await;
                if request.action == "authorize" {
                    if !order.contains(&request.payment) {
                        order.push(request.payment.clone());
                    }
                    if let Ok(answer) = &answer
                        && let Some(id) = answer.body["payment"]["id"].as_str()
                    {
                        ids.insert(request.payment.clone(), id.to_owned());
                    }
                }
                steps.push(
                    money_trace(number, request.action, &request.payment, answer).map_err(fail)?,
                );
                sent.insert(number, request);
            }
            Plan::Repeat {
                step: earlier,
                amount,
            } => {
                let earlier = sent.get(&earlier).ok_or_else(|| RunError::Invalid {
                    scenario: scenario.name.clone(),
                    step: number,
                    problem: format!("step {earlier} sent nothing to repeat"),
                })?;
                let mut body = earlier.body.clone();
                if let Some(amount) = amount {
                    body["amount"] = json!(amount);
                }
                let answer = client
                    .post_money(&earlier.path, &body, Some(earlier.key.clone()))
                    .await;
                steps.push(money_trace(number, "repeat", &earlier.payment, answer).map_err(fail)?);
            }
            Plan::Wait(payment) => {
                let id = ids.get(payment).cloned().ok_or_else(|| RunError::Invalid {
                    scenario: scenario.name.clone(),
                    step: number,
                    problem: format!(
                        "payment {payment} was refused, so there is nothing to wait for"
                    ),
                })?;
                steps.push(
                    wait(client, number, payment, &id, options.wait_timeout)
                        .await
                        .map_err(fail)?,
                );
            }
        }
    }
    let mut payments = Vec::new();
    for name in &order {
        if let Some(id) = ids.get(name) {
            let trace =
                payment_trace(client, name, id)
                    .await
                    .map_err(|source| RunError::Client {
                        scenario: scenario.name.clone(),
                        step: scenario.steps.len(),
                        source,
                    })?;
            payments.push(trace);
        }
    }
    Ok(Trace {
        scenario: scenario.name.clone(),
        steps,
        payments,
    })
}

fn operation_trace(op: &Value) -> OperationTrace {
    OperationTrace {
        kind: op["kind"].as_str().unwrap_or_default().to_owned(),
        state: op["state"].as_str().unwrap_or_default().to_owned(),
        amount: op["amount"].as_i64().unwrap_or_default(),
        decline_code: op["decline_code"].as_str().map(str::to_owned),
    }
}

/// A money-moving step's trace. A refusal is part of the trace; no answer at all is not.
fn money_trace(
    number: usize,
    action: &str,
    payment: &str,
    answer: Result<Answer, ClientError>,
) -> Result<StepTrace, ClientError> {
    match answer {
        Ok(answer) => {
            let operation = operation_trace(&answer.body["operation"]);
            Ok(StepTrace {
                step: number,
                action: action.to_owned(),
                payment: payment.to_owned(),
                http: Some(answer.status),
                result: operation.state.clone(),
                replayed: answer.body["replayed"].as_bool(),
                payment_status: answer.body["payment"]["status"].as_str().map(str::to_owned),
                operation: Some(operation),
            })
        }
        Err(ClientError::Refused { status, code, .. }) => Ok(StepTrace {
            step: number,
            action: action.to_owned(),
            payment: payment.to_owned(),
            http: Some(status),
            result: format!("refused:{code}"),
            replayed: None,
            payment_status: None,
            operation: None,
        }),
        Err(other) => Err(other),
    }
}

/// Polls the payment until nothing of it is open, at most `timeout`.
async fn wait(
    client: &Client,
    number: usize,
    payment: &str,
    id: &str,
    timeout: Duration,
) -> Result<StepTrace, ClientError> {
    let started = Instant::now();
    let mut pause = Duration::from_millis(100);
    loop {
        let answer = client.get(&format!("/v1/payments/{id}")).await?;
        let open = answer.body["operations"].as_array().is_some_and(|ops| {
            ops.iter()
                .any(|op| matches!(op["state"].as_str(), Some("pending" | "uncertain")))
        });
        if !open || started.elapsed() + pause > timeout {
            return Ok(StepTrace {
                step: number,
                action: "wait".to_owned(),
                payment: payment.to_owned(),
                http: None,
                result: if open { "still_open" } else { "settled" }.to_owned(),
                replayed: None,
                payment_status: answer.body["payment"]["status"].as_str().map(str::to_owned),
                operation: None,
            });
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(1));
    }
}

async fn payment_trace(client: &Client, name: &str, id: &str) -> Result<PaymentTrace, ClientError> {
    let detail = client.get(&format!("/v1/payments/{id}")).await?.body;
    let events = client
        .get(&format!("/v1/events?payment_id={id}&limit=100"))
        .await?
        .body;
    let journal = client
        .get(&format!("/v1/payments/{id}/journal"))
        .await?
        .body;
    let payment = &detail["payment"];
    let mut events: Vec<EventTrace> = events["events"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|e| EventTrace {
                    kind: e["kind"].as_str().unwrap_or_default().to_owned(),
                    sequence: e["sequence"].as_i64().unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    events.sort_by_key(|e| e.sequence);
    let journal = journal["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| EntryTrace {
                    recipe: entry["recipe"].as_str().unwrap_or_default().to_owned(),
                    lines: entry["lines"]
                        .as_array()
                        .map(|lines| {
                            lines
                                .iter()
                                .map(|line| {
                                    (
                                        line[0].as_str().unwrap_or_default().to_owned(),
                                        line[1].as_i64().unwrap_or_default(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let number = |field: &str| payment[field].as_i64().unwrap_or_default();
    Ok(PaymentTrace {
        payment: name.to_owned(),
        status: payment["status"].as_str().unwrap_or_default().to_owned(),
        currency: payment["currency"].as_str().unwrap_or_default().to_owned(),
        amount: number("amount"),
        authorized: number("authorized"),
        captured: number("captured"),
        voided: number("voided"),
        refunded: number("refunded"),
        operations: detail["operations"]
            .as_array()
            .map(|ops| ops.iter().map(operation_trace).collect())
            .unwrap_or_default(),
        events,
        journal,
    })
}

/// The differences between an expected and an actual trace, as plain lines (empty when
/// equal).
pub fn differences(expected: &Trace, actual: &Trace) -> Vec<String> {
    let mut out = Vec::new();
    let longest = expected.steps.len().max(actual.steps.len());
    for i in 0..longest {
        match (expected.steps.get(i), actual.steps.get(i)) {
            (Some(e), Some(a)) if e == a => {}
            (e, a) => out.push(format!(
                "step {}: expected {}, got {}",
                i + 1,
                serde_json::to_string(&e).unwrap_or_default(),
                serde_json::to_string(&a).unwrap_or_default()
            )),
        }
    }
    let longest = expected.payments.len().max(actual.payments.len());
    for i in 0..longest {
        match (expected.payments.get(i), actual.payments.get(i)) {
            (Some(e), Some(a)) if e == a => {}
            (e, a) => out.push(format!(
                "payment {}: expected {}, got {}",
                e.or(a).map_or("?", |p| p.payment.as_str()),
                serde_json::to_string(&e).unwrap_or_default(),
                serde_json::to_string(&a).unwrap_or_default()
            )),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_scenario_and_expected_trace_parses() {
        let mut names = std::collections::BTreeSet::new();
        for entry in LIBRARY {
            let scenario = entry.scenario().unwrap();
            assert_eq!(scenario.name, entry.name);
            let trace = entry.expected_trace().unwrap();
            assert_eq!(trace.scenario, entry.name);
            assert_eq!(trace.steps.len(), scenario.steps.len(), "{}", entry.name);
            assert!(names.insert(entry.name), "{} listed twice", entry.name);
        }
    }

    #[test]
    fn the_library_lists_every_file_in_the_directory() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/scenarios");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> = LIBRARY.iter().map(|e| e.name.to_owned()).collect();
        listed.sort();
        assert_eq!(
            on_disk, listed,
            "a scenario file is missing from LIBRARY, or the reverse"
        );
    }

    #[test]
    fn invalid_scenarios_are_refused_with_the_step_named() {
        let cases = [
            (
                r#"{"name":"x","summary":"s","steps":[{"do":"capture","payment":"a","amount":1}]}"#,
                "not authorized before",
            ),
            (
                r#"{"name":"x","summary":"s","steps":[{"do":"authorize","payment":"a","amount":1,"currency":"GBP","scenario":"approve"}]}"#,
                "currency",
            ),
            (
                r#"{"name":"x","summary":"s","steps":[{"do":"authorize","payment":"a","amount":1,"currency":"USD","scenario":"teleport"}]}"#,
                "unknown sandbox scenario",
            ),
            (
                r#"{"name":"x","summary":"s","steps":[{"do":"authorize","payment":"a","amount":1,"currency":"USD","scenario":"approve"},{"do":"repeat","step":2}]}"#,
                "not an earlier",
            ),
            (r#"{"name":"X","summary":"s","steps":[]}"#, "name"),
            (
                r#"{"name":"x","summary":"s","steps":[{"do":"teleport"}]}"#,
                "not a scenario",
            ),
            (
                r#"{"name":"x","summary":"s","steps":[],"extra":1}"#,
                "not a scenario",
            ),
        ];
        for (text, expected) in cases {
            let error = parse(text).unwrap_err().to_string();
            assert!(error.contains(expected), "{text}: {error}");
        }
    }

    #[test]
    fn run_ids_are_letters_only() {
        assert!(valid_run_id(&new_run_id()));
        assert!(!valid_run_id(""));
        assert!(!valid_run_id("abc1"));
        assert!(!valid_run_id("ABC"));
        assert!(!valid_run_id(&"a".repeat(41)));
    }
}
