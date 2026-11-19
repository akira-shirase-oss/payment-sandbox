//! Test-only fault rules: make the next matching operation slow, lose its reply, or find the
//! network unavailable. Held in memory, bounded, and never part of the network's durable
//! record. The control API listens on loopback only, like everything else (D-023).

use std::sync::Mutex;

use sandbox_domain::OperationKind;
use serde::Deserialize;
use uuid::Uuid;

/// Most rules held at once.
pub const MAX_RULES: usize = 64;

/// What a rule does to the operation it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum FaultAction {
    /// Execute and record, then answer after `delay_ms` (a slow network).
    Delay {
        /// Delay before answering.
        delay_ms: u64,
    },
    /// Execute and record, then hold the reply past any client deadline (a lost reply).
    LostReply,
    /// Answer 503 without executing.
    Unavailable,
}

/// A rule: matches by kind and/or parent authorization, `count` times. (`flatten` cannot be
/// combined with `deny_unknown_fields` in serde, so unknown fields are ignored here.)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FaultRule {
    /// Operation kind to match (any when absent).
    pub kind: Option<OperationKind>,
    /// Parent authorization (or, for an authorize, the operation itself) to match.
    pub target: Option<Uuid>,
    /// The action.
    #[serde(flatten)]
    pub action: FaultAction,
    /// How many matching operations it applies to.
    pub count: u32,
}

/// The rule set.
#[derive(Debug, Default)]
pub struct Faults(Mutex<Vec<FaultRule>>);

/// Too many rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("at most 64 fault rules may be held")]
pub struct TooManyRules;

impl Faults {
    /// Adds a rule.
    pub fn add(&self, rule: FaultRule) -> Result<(), TooManyRules> {
        let mut rules = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if rules.len() >= MAX_RULES {
            return Err(TooManyRules);
        }
        rules.push(rule);
        Ok(())
    }

    /// Removes every rule.
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// Takes one use of the first rule matching `kind` and `target`.
    pub fn take(&self, kind: OperationKind, target: Uuid) -> Option<FaultAction> {
        let mut rules = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = rules.iter().position(|rule| {
            rule.count > 0
                && rule.kind.is_none_or(|k| k == kind)
                && rule.target.is_none_or(|t| t == target)
        })?;
        let rule = &mut rules[index];
        rule.count = rule.count.saturating_sub(1);
        let action = rule.action;
        if rule.count == 0 {
            rules.remove(index);
        }
        Some(action)
    }
}

/// Crash points for the durability drills (tests/drills.rs), compiled in only with the
/// `fault-injection` feature; otherwise an empty inline function. `PS_SIM_FAULT=<point>`
/// aborts the process the `PS_SIM_FAULT_AFTER`-th time the point is reached (default 1) —
/// no destructors, no flush, like a power cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// A batch of commands is run and staged, its commit not yet durable.
    StoreCommit,
}

impl Point {
    /// The `PS_SIM_FAULT` value naming this point.
    pub const fn name(self) -> &'static str {
        match self {
            Self::StoreCommit => "store_commit",
        }
    }
}

/// Aborts at `point` per `PS_SIM_FAULT`/`PS_SIM_FAULT_AFTER` (fault-injection only).
#[cfg(feature = "fault-injection")]
pub fn point(point: Point) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    static FAULT: OnceLock<(Option<String>, Option<String>)> = OnceLock::new();
    static HITS: AtomicU64 = AtomicU64::new(0);
    let (armed, after) = FAULT.get_or_init(|| {
        (
            std::env::var("PS_SIM_FAULT").ok(),
            std::env::var("PS_SIM_FAULT_AFTER").ok(),
        )
    });
    if armed.as_deref() == Some(point.name()) {
        let want: u64 = after.as_deref().and_then(|v| v.parse().ok()).unwrap_or(1);
        if HITS.fetch_add(1, Ordering::SeqCst).saturating_add(1) >= want {
            eprintln!("fault injection: aborting at {}", point.name());
            std::process::abort();
        }
    }
}

/// No-op outside fault-injection builds.
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub const fn point(_point: Point) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_match_by_kind_and_target_and_expire() {
        let faults = Faults::default();
        let target = Uuid::from_u128(5);
        faults
            .add(FaultRule {
                kind: Some(OperationKind::Capture),
                target: Some(target),
                action: FaultAction::LostReply,
                count: 2,
            })
            .unwrap();
        assert_eq!(faults.take(OperationKind::Refund, target), None);
        assert_eq!(
            faults.take(OperationKind::Capture, Uuid::from_u128(6)),
            None
        );
        assert_eq!(
            faults.take(OperationKind::Capture, target),
            Some(FaultAction::LostReply)
        );
        assert_eq!(
            faults.take(OperationKind::Capture, target),
            Some(FaultAction::LostReply)
        );
        assert_eq!(faults.take(OperationKind::Capture, target), None);
    }

    #[test]
    fn the_rule_set_is_bounded() {
        let faults = Faults::default();
        let rule = FaultRule {
            kind: None,
            target: None,
            action: FaultAction::Unavailable,
            count: 1,
        };
        for _ in 0..MAX_RULES {
            faults.add(rule.clone()).unwrap();
        }
        assert_eq!(faults.add(rule), Err(TooManyRules));
    }

    #[test]
    fn rules_parse_from_json() {
        let rule: FaultRule =
            serde_json::from_str(r#"{"kind": "capture", "target": null, "action": "delay", "delay_ms": 800, "count": 1}"#)
                .unwrap();
        assert_eq!(rule.action, FaultAction::Delay { delay_ms: 800 });
    }
}
