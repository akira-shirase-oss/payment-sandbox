//! `sandbox-cli`: the Payment Sandbox from a terminal. **SIMULATED MONEY ONLY.**

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use sandbox_cli::client::{Answer, Client, ClientError, RetryPolicy};
use sandbox_cli::config::{Config, DEFAULT_API_URL};
use sandbox_cli::scenario::{self, RunOptions};
use sandbox_domain::money::format_minor;
use sandbox_domain::{Amount, Currency};
use serde_json::{Value, json};

const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\nSIMULATED MONEY ONLY: Payment Sandbox moves no real money and accepts no real cards."
);

const AFTER_HELP: &str = "\
Examples:
  export PS_API_KEY=sk_sim_...                       # a key of your project
  sandbox-cli status
  sandbox-cli payments create --amount 42.00 --currency USD
  sandbox-cli payments capture <payment id> --amount 42.00
  sandbox-cli payments refund <payment id> --amount 12.00
  sandbox-cli ledger explain USD merchant_pending
  sandbox-cli scenarios run --all --check
  sandbox-cli reset

Exit status:
  0  done
  1  the sandbox refused the request (the message says why)
  2  wrong usage or configuration
  3  the simulated network declined, or never executed the operation
  4  the outcome is not known yet (uncertain): check again before trying anything new
  5  the sandbox could not be reached, or the URL is not a Payment Sandbox

SIMULATED MONEY ONLY: no real cards, balances or banking connection.";

/// The Payment Sandbox from a terminal (SIMULATED MONEY ONLY).
#[derive(Debug, Parser)]
#[command(
    name = "sandbox-cli",
    version,
    long_version = LONG_VERSION,
    about = "The Payment Sandbox from a terminal. SIMULATED MONEY ONLY: no real cards, balances or banking connection.",
    after_help = AFTER_HELP,
    propagate_version = true
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct Global {
    /// The sandbox API.
    #[arg(long, env = "PS_API_URL", default_value = DEFAULT_API_URL, global = true)]
    api_url: String,
    #[arg(
        long,
        env = "PS_API_KEY",
        hide_env_values = true,
        global = true,
        help = "Your project's sandbox key (sk_sim_...). Prefer the environment variable: a flag can end up in your shell history. Never printed"
    )]
    key: Option<String>,
    /// Allow an API on another machine (a CI service container, or a shared sandbox you run).
    /// Never allows a real processor's host or key.
    #[arg(long, env = "PS_ALLOW_NON_LOCAL", global = true)]
    allow_non_local: bool,
    /// Print the API's JSON instead of plain words.
    #[arg(long, global = true)]
    json: bool,
    /// Longest one command waits for one request, retries included, in seconds.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=600), global = true)]
    timeout: u64,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check that the API is a Payment Sandbox, and show your project and its epoch.
    Status,
    /// Create, capture, void, refund, inspect and wait for payments.
    #[command(subcommand)]
    Payments(Payments),
    /// Balances, a payment's journal entries, and any balance explained line by line.
    #[command(subcommand)]
    Ledger(Ledger),
    /// Run the reconciliation report: the journal against the payments (and the network).
    Reconcile {
        /// Also compare with the simulated network's records, both ways.
        #[arg(long)]
        network: bool,
    },
    /// List webhook events and their deliveries.
    Events {
        /// Only this payment's events.
        #[arg(long)]
        payment: Option<String>,
        /// At most this many (1-500).
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List and run the seeded scenario library.
    #[command(subcommand)]
    Scenarios(Scenarios),
    /// Reset your project's dataset: open a new, empty epoch. Nothing is deleted; it needs an
    /// admin key and the project's slug typed as the confirmation.
    Reset {
        /// The project's slug, typed as the confirmation. Without it you are asked to type it
        /// (in a terminal).
        #[arg(long)]
        confirm: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum Payments {
    /// Authorize a payment (a hold) with a scenario the simulated network acts out.
    Create {
        /// Amount in the currency's units: 42.00 USD, 500 JPY, 1.250 KWD.
        #[arg(long)]
        amount: String,
        /// USD, EUR, JPY or KWD.
        #[arg(long)]
        currency: String,
        /// What the simulated network does.
        #[arg(long, value_enum, default_value_t = NetworkScenario::Approve)]
        scenario: NetworkScenario,
        /// Shown in the dashboard; at most 200 characters.
        #[arg(long)]
        description: Option<String>,
        /// Reuse a key to retry a request whose answer you did not get (never a new one).
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Show a payment and its operations.
    Get {
        /// The payment id.
        id: String,
    },
    /// List the most recent payments.
    List {
        /// At most this many (1-500).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Capture part or all of the hold.
    Capture(MoneyOn),
    /// Refund captured money.
    Refund(MoneyOn),
    /// Release the uncaptured remainder of the hold.
    Void {
        /// The payment id.
        id: String,
        /// Reuse a key to retry a request whose answer you did not get.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Wait until none of the payment's operations is pending or uncertain.
    Wait {
        /// The payment id.
        id: String,
        /// Give up after this many seconds (the answer then says what is still open).
        #[arg(long, default_value_t = 30)]
        wait_secs: u64,
    },
}

#[derive(Debug, Args)]
struct MoneyOn {
    /// The payment id.
    id: String,
    /// Amount in the payment's currency units, e.g. 12.00.
    #[arg(long)]
    amount: String,
    /// Reuse a key to retry a request whose answer you did not get.
    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Ledger {
    /// Every account balance, summed from the journal.
    Balances,
    /// A payment's journal entries.
    Journal {
        /// The payment id.
        payment: String,
    },
    /// The journal lines that sum to one account's balance, page by page.
    Explain {
        /// USD, EUR, JPY or KWD.
        currency: String,
        #[arg(help = "The account: payer_funding, payer_held, merchant_pending, ...")]
        account: String,
        /// Lines per page (1-1000).
        #[arg(long, default_value_t = 100)]
        page: u32,
    },
}

#[derive(Debug, Subcommand)]
enum Scenarios {
    /// List the library.
    List,
    /// Show a scenario's steps.
    Show {
        /// Its name.
        name: String,
    },
    /// Run scenarios and print their traces; --check compares each with its expected trace.
    Run(RunArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Library scenarios to run.
    names: Vec<String>,
    /// Run the whole library.
    #[arg(long, conflicts_with_all = ["names", "file"])]
    all: bool,
    /// Run a scenario from a JSON file instead.
    #[arg(long, conflicts_with = "names")]
    file: Option<std::path::PathBuf>,
    /// Compare each trace with the library's expected one; exit 1 on any difference.
    #[arg(long)]
    check: bool,
    /// Reuse a run's keys (lowercase letters); by default every run is new.
    #[arg(long)]
    run_id: Option<String>,
    /// Longest a wait step waits, in seconds.
    #[arg(long, default_value_t = 30)]
    wait_secs: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
enum NetworkScenario {
    /// Approve.
    Approve,
    /// Decline: insufficient funds.
    DeclineInsufficientFunds,
    /// Decline: do not honor.
    DeclineDoNotHonor,
    /// Decline: expired payment method.
    DeclineExpired,
    /// Approve, then lose the reply (uncertain until the reconciler reads it back).
    LostReply,
    /// Answer 503 without executing (uncertain until a fenced readback).
    Unavailable,
}

impl NetworkScenario {
    fn token(self) -> &'static str {
        match self {
            Self::Approve => "tok_sim_approve",
            Self::DeclineInsufficientFunds => "tok_sim_decline_insufficient_funds",
            Self::DeclineDoNotHonor => "tok_sim_decline_do_not_honor",
            Self::DeclineExpired => "tok_sim_decline_expired",
            Self::LostReply => "tok_sim_lost_reply",
            Self::Unavailable => "tok_sim_unavailable",
        }
    }
}

/// How a command ended; its exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Done = 0,
    Refused = 1,
    Usage = 2,
    Declined = 3,
    Uncertain = 4,
    Unreachable = 5,
}

/// A failure, with the words to print and the exit status.
struct Failure {
    ended: Ended,
    message: String,
}

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        let ended = match &error {
            ClientError::Refused { .. } => Ended::Refused,
            ClientError::Unknown { .. } => Ended::Uncertain,
            ClientError::Unreachable { .. }
            | ClientError::NotASandbox { .. }
            | ClientError::Unreadable { .. } => Ended::Unreachable,
        };
        Self {
            ended,
            message: error.to_string(),
        }
    }
}

fn usage(message: impl Into<String>) -> Failure {
    Failure {
        ended: Ended::Usage,
        message: message.into(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    eprintln!("{}", sandbox_cli::NOTICE);
    match run(cli).await {
        Ok(ended) => ExitCode::from(ended as u8),
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.ended as u8)
        }
    }
}

struct Ctx {
    client: Client,
    json: bool,
}

impl Ctx {
    fn print(&self, value: &Value, words: impl FnOnce() -> String) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            );
        } else {
            println!("{}", words());
        }
    }
}

async fn run(cli: Cli) -> Result<Ended, Failure> {
    let config = Config::new(
        &cli.global.api_url,
        cli.global.key.as_deref(),
        cli.global.allow_non_local,
    )
    .map_err(|e| usage(e.to_string()))?;
    let retry = RetryPolicy {
        deadline: Duration::from_secs(cli.global.timeout),
        ..RetryPolicy::default()
    };
    let ctx = Ctx {
        client: Client::new(config, retry)?,
        json: cli.global.json,
    };
    match cli.command {
        Command::Status => status(&ctx).await,
        Command::Payments(command) => payments(&ctx, command).await,
        Command::Ledger(command) => ledger(&ctx, command).await,
        Command::Reconcile { network } => reconcile(&ctx, network).await,
        Command::Events { payment, limit } => events(&ctx, payment.as_deref(), limit).await,
        Command::Scenarios(command) => scenarios(&ctx, command).await,
        Command::Reset { confirm } => reset(&ctx, confirm).await,
    }
}

async fn status(ctx: &Ctx) -> Result<Ended, Failure> {
    let notice = ctx.client.get("/v1/sandbox").await?.body;
    if notice["simulated"] != json!(true) {
        return Err(Failure::from(ClientError::NotASandbox {
            url: ctx.client.config().api_url.clone(),
        }));
    }
    let project = ctx.client.get("/v1/project").await?.body;
    let both = json!({"sandbox": notice, "project": project["project"]});
    ctx.print(&both, || {
        let p = &project["project"];
        format!(
            "Payment Sandbox at {} (ledger semantics v{})\nProject {} ({}), dataset epoch {}",
            ctx.client.config().api_url,
            notice["ledger_semantics_version"],
            p["name"].as_str().unwrap_or("?"),
            p["slug"].as_str().unwrap_or("?"),
            p["dataset_epoch"]
        )
    });
    Ok(Ended::Done)
}

fn parse_amount(text: &str, currency: Currency) -> Result<i64, Failure> {
    Amount::parse_decimal(text, currency)
        .map(Amount::minor)
        .map_err(|e| {
            usage(format!(
                "{text} is not an amount in {}: {e} (write it like {})",
                currency.code(),
                format_minor(4_200, currency)
            ))
        })
}

fn currency(code: &str) -> Result<Currency, Failure> {
    Currency::from_code(&code.to_ascii_uppercase()).map_err(|_| {
        usage(format!(
            "{code} is not a sandbox currency: use USD, EUR, JPY or KWD"
        ))
    })
}

/// The payment's currency, read from the API (captures and refunds name amounts in it).
async fn payment_currency(ctx: &Ctx, id: &str) -> Result<Currency, Failure> {
    let body = ctx.client.get(&format!("/v1/payments/{id}")).await?.body;
    currency(body["payment"]["currency"].as_str().unwrap_or_default())
}

async fn payments(ctx: &Ctx, command: Payments) -> Result<Ended, Failure> {
    match command {
        Payments::Create {
            amount,
            currency: code,
            scenario,
            description,
            idempotency_key,
        } => {
            let currency = currency(&code)?;
            let minor = parse_amount(&amount, currency)?;
            let mut body =
                json!({"amount": minor, "currency": currency.code(), "token": scenario.token()});
            if let Some(description) = description {
                body["description"] = json!(description);
            }
            let answer = ctx
                .client
                .post_money("/v1/payments", &body, idempotency_key)
                .await?;
            Ok(report_operation(ctx, &answer))
        }
        Payments::Capture(on) => {
            let minor = parse_amount(&on.amount, payment_currency(ctx, &on.id).await?)?;
            let answer = ctx
                .client
                .post_money(
                    &format!("/v1/payments/{}/captures", on.id),
                    &json!({"amount": minor}),
                    on.idempotency_key,
                )
                .await?;
            Ok(report_operation(ctx, &answer))
        }
        Payments::Refund(on) => {
            let minor = parse_amount(&on.amount, payment_currency(ctx, &on.id).await?)?;
            let answer = ctx
                .client
                .post_money(
                    &format!("/v1/payments/{}/refunds", on.id),
                    &json!({"amount": minor}),
                    on.idempotency_key,
                )
                .await?;
            Ok(report_operation(ctx, &answer))
        }
        Payments::Void {
            id,
            idempotency_key,
        } => {
            let answer = ctx
                .client
                .post_money(
                    &format!("/v1/payments/{id}/void"),
                    &json!({}),
                    idempotency_key,
                )
                .await?;
            Ok(report_operation(ctx, &answer))
        }
        Payments::Get { id } => {
            let body = ctx.client.get(&format!("/v1/payments/{id}")).await?.body;
            ctx.print(&body, || describe_payment(&body));
            Ok(Ended::Done)
        }
        Payments::List { limit } => {
            let body = ctx
                .client
                .get(&format!("/v1/payments?limit={}", limit.clamp(1, 500)))
                .await?
                .body;
            ctx.print(&body, || {
                let rows = body["payments"].as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    return "No payments in this epoch yet. Create one with: sandbox-cli payments create --amount 42.00 --currency USD".to_owned();
                }
                rows.iter()
                    .map(|p| {
                        format!(
                            "{}  {:>14}  {}",
                            p["id"].as_str().unwrap_or("?"),
                            money(p["amount"].as_i64(), p["currency"].as_str()),
                            p["status"].as_str().unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(Ended::Done)
        }
        Payments::Wait { id, wait_secs } => wait(ctx, &id, wait_secs).await,
    }
}

fn money(minor: Option<i64>, code: Option<&str>) -> String {
    match (minor, code.and_then(|c| Currency::from_code(c).ok())) {
        (Some(minor), Some(currency)) => {
            format!("{} {}", format_minor(minor, currency), currency.code())
        }
        _ => "?".to_owned(),
    }
}

/// Plain words for a money-moving answer; the exit status says what happened.
fn report_operation(ctx: &Ctx, answer: &Answer) -> Ended {
    let op = &answer.body["operation"];
    let payment = &answer.body["payment"];
    let state = op["state"].as_str().unwrap_or("?");
    let ended = match state {
        "succeeded" => Ended::Done,
        "declined" | "not_executed" => Ended::Declined,
        _ => Ended::Uncertain,
    };
    ctx.print(&answer.body, || {
        let kind = op["kind"].as_str().unwrap_or("operation");
        let amount = money(op["amount"].as_i64(), payment["currency"].as_str());
        let headline = match state {
            "succeeded" => format!("{kind} of {amount} succeeded"),
            "declined" => format!(
                "{kind} of {amount} was declined by the simulated network ({})",
                op["decline_code"].as_str().unwrap_or("no reason given")
            ),
            "not_executed" => format!(
                "{kind} of {amount} was never executed by the simulated network: nothing moved"
            ),
            _ => format!(
                "{kind} of {amount} is UNCERTAIN: the network's answer has not arrived, so this is not done yet. The reconciler reads it back; run `sandbox-cli payments wait {}`",
                payment["id"].as_str().unwrap_or("<id>")
            ),
        };
        let mut lines = vec![
            headline,
            format!(
                "payment {}  status {}",
                payment["id"].as_str().unwrap_or("?"),
                payment["status"].as_str().unwrap_or("?")
            ),
        ];
        if answer.body["replayed"] == json!(true) {
            lines.push(
                "(a repeat: the sandbox answered from its record of the first request)".to_owned(),
            );
        }
        if let Some(key) = &answer.idempotency_key {
            lines.push(format!(
                "Idempotency-Key {key}: to retry this request, reuse it with --idempotency-key"
            ));
        }
        lines.join("\n")
    });
    ended
}

fn describe_payment(body: &Value) -> String {
    let p = &body["payment"];
    let currency = p["currency"].as_str();
    let mut lines = vec![
        format!(
            "payment {}  {}  status {}",
            p["id"].as_str().unwrap_or("?"),
            money(p["amount"].as_i64(), currency),
            p["status"].as_str().unwrap_or("?")
        ),
        format!(
            "authorized {}  captured {}  voided {}  refunded {}",
            money(p["authorized"].as_i64(), currency),
            money(p["captured"].as_i64(), currency),
            money(p["voided"].as_i64(), currency),
            money(p["refunded"].as_i64(), currency)
        ),
    ];
    for op in body["operations"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  {:<9} {:>14}  {}{}",
            op["kind"].as_str().unwrap_or("?"),
            money(op["amount"].as_i64(), currency),
            op["state"].as_str().unwrap_or("?"),
            op["decline_code"]
                .as_str()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default()
        ));
    }
    lines.join("\n")
}

async fn wait(ctx: &Ctx, id: &str, wait_secs: u64) -> Result<Ended, Failure> {
    let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs.clamp(1, 600));
    let mut pause = Duration::from_millis(100);
    loop {
        let body = ctx.client.get(&format!("/v1/payments/{id}")).await?.body;
        let open: Vec<&Value> = body["operations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|op| matches!(op["state"].as_str(), Some("pending" | "uncertain")))
            .collect();
        if open.is_empty() {
            ctx.print(&body, || {
                format!(
                    "Nothing of it is open any more.\n{}",
                    describe_payment(&body)
                )
            });
            return Ok(Ended::Done);
        }
        if std::time::Instant::now() + pause > deadline {
            let count = open.len();
            ctx.print(&body, || {
                format!(
                    "Still open after {wait_secs} s: {count} operation(s) pending or uncertain. They are not done; the reconciler keeps reading the network's record back. Wait again later.\n{}",
                    describe_payment(&body)
                )
            });
            return Ok(Ended::Uncertain);
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(1));
    }
}

async fn ledger(ctx: &Ctx, command: Ledger) -> Result<Ended, Failure> {
    match command {
        Ledger::Balances => {
            let body = ctx.client.get("/v1/balances").await?.body;
            ctx.print(&body, || {
                let rows = body["balances"].as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    return "Every balance is zero in this epoch: no journal entries yet."
                        .to_owned();
                }
                rows.iter()
                    .map(|b| {
                        format!(
                            "{} {:<20} {:>16}",
                            b["currency"].as_str().unwrap_or("?"),
                            b["account"].as_str().unwrap_or("?"),
                            b["balance"].as_str().unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Ledger::Journal { payment } => {
            let body = ctx
                .client
                .get(&format!("/v1/payments/{payment}/journal"))
                .await?
                .body;
            ctx.print(&body, || {
                body["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        let lines = e["lines"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|l| {
                                format!("    {:<20} {:>16}", l[0].as_str().unwrap_or("?"), l[1])
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!(
                            "#{} {} {}\n{lines}",
                            e["seq"],
                            e["recipe"].as_str().unwrap_or("?"),
                            e["currency"].as_str().unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Ledger::Explain {
            currency: code,
            account,
            page,
        } => return explain(ctx, &code, &account, page).await,
    }
    Ok(Ended::Done)
}

/// Every page of an explanation, checking that its lines sum to the balance.
async fn explain(ctx: &Ctx, code: &str, account: &str, page: u32) -> Result<Ended, Failure> {
    let currency = currency(code)?;
    let base = format!(
        "/v1/balances/{}/{account}/explain?limit={}",
        currency.code(),
        page.clamp(1, 1_000)
    );
    let mut cursor: Option<String> = None;
    let mut sum: i64 = 0;
    let mut pages = Vec::new();
    // Bounded: at most 10,000 pages of up to 1,000 lines.
    for _ in 0..10_000 {
        let path = match &cursor {
            Some(c) => format!("{base}&cursor={c}"),
            None => base.clone(),
        };
        let body = ctx.client.get(&path).await?.body;
        for line in body["explanation"]["lines"]
            .as_array()
            .into_iter()
            .flatten()
        {
            sum = sum.saturating_add(line["amount_minor"].as_i64().unwrap_or_default());
            if !ctx.json {
                println!(
                    "#{:<8} {:<9} {:>16}  running {:>16}",
                    line["seq"],
                    line["recipe"].as_str().unwrap_or("?"),
                    format_minor(line["amount_minor"].as_i64().unwrap_or_default(), currency),
                    format_minor(line["running_minor"].as_i64().unwrap_or_default(), currency)
                );
            }
        }
        cursor = body["explanation"]["next"]["cursor"]
            .as_str()
            .map(str::to_owned);
        let balance = body["explanation"]["balance_minor"]
            .as_i64()
            .unwrap_or_default();
        let presented = body["balance"].as_str().unwrap_or("?").to_owned();
        pages.push(body);
        if cursor.is_none() {
            let explained = sum == balance;
            ctx.print(&json!({"pages": pages, "lines_sum_minor": sum, "explained": explained}), || {
                if explained {
                    format!(
                        "Explained: the lines sum to the balance, {presented} {} (journal sign {}: debits +, credits -).",
                        currency.code(),
                        format_minor(sum, currency)
                    )
                } else {
                    format!("NOT explained: the lines sum to {} but the balance is {}. Run sandbox-cli reconcile.", format_minor(sum, currency), format_minor(balance, currency))
                }
            });
            return Ok(if explained {
                Ended::Done
            } else {
                Ended::Refused
            });
        }
    }
    Err(Failure {
        ended: Ended::Refused,
        message: "the explanation has more than 10,000 pages; use a larger --page".to_owned(),
    })
}

async fn reconcile(ctx: &Ctx, network: bool) -> Result<Ended, Failure> {
    let path = if network {
        "/v1/reconciliation?network=true"
    } else {
        "/v1/reconciliation"
    };
    let body = ctx.client.get(path).await?.body;
    let clean = body["clean"] == json!(true);
    ctx.print(&body, || {
        let r = &body["report"];
        let open = r["open_operations"].as_i64().unwrap_or_default();
        let mut lines = vec![if clean {
            "Clean: every balance equals the sum of its journal entries, and every payment its entries.".to_owned()
        } else {
            "NOT clean: the report below names each mismatch.".to_owned()
        }];
        lines.push(format!(
            "payments checked {}, journal entries checked {}, open operations {open}",
            r["payments_checked"], r["entries_checked"]
        ));
        if open > 0 {
            lines.push("Open operations are not failures: the reconciler resolves them by reading the network's record back.".to_owned());
        }
        if !clean {
            lines.push(serde_json::to_string_pretty(&body).unwrap_or_default());
        }
        lines.join("\n")
    });
    Ok(if clean { Ended::Done } else { Ended::Refused })
}

async fn events(ctx: &Ctx, payment: Option<&str>, limit: u32) -> Result<Ended, Failure> {
    let mut path = format!("/v1/events?limit={}", limit.clamp(1, 500));
    if let Some(payment) = payment {
        path = format!("{path}&payment_id={payment}");
    }
    let body = ctx.client.get(&path).await?.body;
    ctx.print(&body, || {
        let rows = body["events"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            return "No events yet: every resolved operation records one.".to_owned();
        }
        rows.iter()
            .map(|e| {
                let deliveries = e["deliveries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|d| format!("{} after {} attempt(s)", d[1].as_str().unwrap_or("?"), d[2]))
                    .collect::<Vec<_>>();
                format!(
                    "{}  #{} {:<24} {}",
                    e["payment_id"].as_str().unwrap_or("?"),
                    e["sequence"],
                    e["kind"].as_str().unwrap_or("?"),
                    if deliveries.is_empty() {
                        "no endpoint".to_owned()
                    } else {
                        deliveries.join(", ")
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(Ended::Done)
}

async fn scenarios(ctx: &Ctx, command: Scenarios) -> Result<Ended, Failure> {
    match command {
        Scenarios::List => {
            let list: Vec<Value> = scenario::LIBRARY
                .iter()
                .filter_map(|e| e.scenario().ok())
                .map(|s| json!({"name": s.name, "summary": s.summary, "steps": s.steps.len()}))
                .collect();
            ctx.print(&json!(list), || {
                list.iter()
                    .map(|s| {
                        format!(
                            "{:<28} {}",
                            s["name"].as_str().unwrap_or("?"),
                            s["summary"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(Ended::Done)
        }
        Scenarios::Show { name } => {
            let entry = scenario::find(&name).ok_or_else(|| unknown_scenario(&name))?;
            let parsed = entry.scenario().map_err(|e| usage(e.to_string()))?;
            ctx.print(&json!(parsed), || {
                serde_json::to_string_pretty(&parsed).unwrap_or_default()
            });
            Ok(Ended::Done)
        }
        Scenarios::Run(args) => run_scenarios(ctx, args).await,
    }
}

/// `scenarios run`: each chosen scenario under a fresh (or the given) run id.
async fn run_scenarios(ctx: &Ctx, args: RunArgs) -> Result<Ended, Failure> {
    let RunArgs {
        names,
        all,
        file,
        check,
        run_id,
        wait_secs,
    } = args;
    if let Some(id) = &run_id
        && !scenario::valid_run_id(id)
    {
        return Err(usage("a run id is 1-40 lowercase letters (no digits)"));
    }
    let mut chosen = Vec::new();
    if let Some(path) = file {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| usage(format!("cannot read {}: {e}", path.display())))?;
        chosen.push((
            scenario::parse(&text).map_err(|e| usage(e.to_string()))?,
            None,
        ));
    } else if all {
        for entry in scenario::LIBRARY {
            chosen.push((
                entry.scenario().map_err(|e| usage(e.to_string()))?,
                Some(entry),
            ));
        }
    } else if names.is_empty() {
        return Err(usage(
            "name a scenario (sandbox-cli scenarios list), or pass --all or --file",
        ));
    } else {
        for name in &names {
            let entry = scenario::find(name).ok_or_else(|| unknown_scenario(name))?;
            chosen.push((
                entry.scenario().map_err(|e| usage(e.to_string()))?,
                Some(entry),
            ));
        }
    }
    if check && chosen.iter().any(|(_, entry)| entry.is_none()) {
        return Err(usage(
            "--check compares with the library's expected traces; a scenario from --file has none",
        ));
    }
    let options = RunOptions {
        wait_timeout: Duration::from_secs(wait_secs.clamp(1, 600)),
    };
    let mut worst = Ended::Done;
    let mut traces = Vec::new();
    for (definition, entry) in chosen {
        let id = run_id.clone().unwrap_or_else(scenario::new_run_id);
        let trace = scenario::run(&ctx.client, &definition, &id, options)
            .await
            .map_err(|e| Failure {
                ended: match &e {
                    scenario::RunError::Client {
                        source: ClientError::Unknown { .. },
                        ..
                    } => Ended::Uncertain,
                    scenario::RunError::Client {
                        source: ClientError::Refused { .. },
                        ..
                    } => Ended::Refused,
                    scenario::RunError::Client { .. } => Ended::Unreachable,
                    scenario::RunError::Invalid { .. } => Ended::Usage,
                },
                message: e.to_string(),
            })?;
        let differences = match (check, entry) {
            (true, Some(entry)) => {
                let expected = entry.expected_trace().map_err(|e| usage(e.to_string()))?;
                Some(scenario::differences(&expected, &trace))
            }
            _ => None,
        };
        if differences.as_ref().is_some_and(|d| !d.is_empty()) {
            worst = Ended::Refused;
        }
        if !ctx.json {
            println!(
                "{}",
                describe_trace(&definition, &trace, differences.as_deref())
            );
        }
        traces.push(json!({"trace": trace, "run_id": id, "differences": differences}));
    }
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&traces).unwrap_or_default()
        );
    }
    Ok(worst)
}

fn unknown_scenario(name: &str) -> Failure {
    usage(format!(
        "no library scenario is called {name}; list them with: sandbox-cli scenarios list"
    ))
}

fn describe_trace(
    definition: &scenario::Scenario,
    trace: &scenario::Trace,
    differences: Option<&[String]>,
) -> String {
    let mut lines = vec![format!("{}: {}", definition.name, definition.summary)];
    for step in &trace.steps {
        lines.push(format!(
            "  {:>2} {:<9} {:<8} {:>5} {:<34} {}",
            step.step,
            step.action,
            step.payment,
            step.http.map(|h| h.to_string()).unwrap_or_default(),
            step.result,
            step.payment_status
                .as_deref()
                .map(|s| format!("payment {s}"))
                .unwrap_or_default()
        ));
    }
    for payment in &trace.payments {
        lines.push(format!(
            "  {}: {} — {} event(s), {} journal entr{}",
            payment.payment,
            payment.status,
            payment.events.len(),
            payment.journal.len(),
            if payment.journal.len() == 1 {
                "y"
            } else {
                "ies"
            }
        ));
    }
    match differences {
        Some([]) => lines.push("  matches the library's expected trace".to_owned()),
        Some(found) => {
            lines.push("  DIFFERS from the library's expected trace:".to_owned());
            lines.extend(found.iter().map(|d| format!("    {d}")));
        }
        None => {}
    }
    lines.join("\n")
}

async fn reset(ctx: &Ctx, confirm: Option<String>) -> Result<Ended, Failure> {
    let project = ctx.client.get("/v1/project").await?.body;
    let slug = project["project"]["slug"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let epoch = project["project"]["dataset_epoch"]
        .as_i64()
        .unwrap_or_default();
    let typed = match confirm {
        Some(typed) => typed,
        None if std::io::stdin().is_terminal() => {
            eprint!(
                "This opens epoch {} of project {slug}: payments, balances and explanations start empty; nothing is deleted.\nType the project's slug ({slug}) to confirm: ",
                epoch + 1
            );
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| usage(format!("could not read the confirmation: {e}")))?;
            line.trim().to_owned()
        }
        None => {
            return Err(usage(format!(
                "a reset needs the project's slug typed as the confirmation: pass --confirm {slug}"
            )));
        }
    };
    if typed != slug {
        return Err(Failure {
            ended: Ended::Refused,
            message: format!("that is not the project's slug ({slug}); nothing was reset"),
        });
    }
    match ctx
        .client
        .post(
            "/v1/project/reset",
            &json!({"confirm": typed, "epoch": epoch}),
        )
        .await
    {
        Ok(answer) => {
            ctx.print(&answer.body, || {
                let r = &answer.body["reset"];
                format!(
                    "Reset: epoch {} is now current and empty. {} payment(s) of epoch {} stay archived in the ledger; nothing was deleted.",
                    r["to_epoch"], r["archived_payments"], r["from_epoch"]
                )
            });
            Ok(Ended::Done)
        }
        Err(ClientError::Unreachable { cause, .. }) => Err(Failure {
            ended: Ended::Uncertain,
            message: format!(
                "no answer to the reset ({cause}): it may or may not have happened. Run `sandbox-cli status`: epoch {} means it did, {epoch} that it did not",
                epoch + 1
            ),
        }),
        Err(other) => Err(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }
}
