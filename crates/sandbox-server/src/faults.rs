//! Crash points for the recovery drills. Compiled in only with the `fault-injection`
//! feature; otherwise every point is an empty inline function. With the feature, setting
//! `PS_FAULT=<point>` (optionally `PS_FAULT_KIND=<operation kind>`) aborts the process at
//! that point — no destructors, no flush, no graceful anything, which is what a power cut
//! or OOM kill looks like. `PS_FAULT=reconciler_panic` instead makes the reconciler's first
//! sweep panic (a defect in a background task, not a crash of the process).

use sandbox_domain::OperationKind;

/// Where the process may be killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// The operation's identity is committed; the network has not been called.
    BeforeNetwork,
    /// The network answered (and recorded its result); the answer is not stored yet.
    AfterNetwork,
}

impl Point {
    /// The `PS_FAULT` value naming this point.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BeforeNetwork => "before_network",
            Self::AfterNetwork => "after_network",
        }
    }
}

/// Panics once, the first time it is reached, when `PS_FAULT=reconciler_panic`
/// (fault-injection builds only): a background task's defect.
#[cfg(feature = "fault-injection")]
pub fn reconciler_panic() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRED: AtomicBool = AtomicBool::new(false);
    if std::env::var("PS_FAULT").ok().as_deref() == Some("reconciler_panic")
        && !FIRED.swap(true, Ordering::SeqCst)
    {
        panic!("fault injection: the reconciler panics");
    }
}

/// No-op outside fault-injection builds.
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub const fn reconciler_panic() {}

/// Aborts the process when `PS_FAULT` names `point` (fault-injection builds only).
#[cfg(feature = "fault-injection")]
pub fn point(point: Point, kind: OperationKind) {
    use std::sync::OnceLock;
    static FAULT: OnceLock<(Option<String>, Option<String>)> = OnceLock::new();
    let (armed, armed_kind) = FAULT.get_or_init(|| {
        (
            std::env::var("PS_FAULT").ok(),
            std::env::var("PS_FAULT_KIND").ok(),
        )
    });
    let kind_matches = armed_kind.as_deref().is_none_or(|k| k == kind.code());
    if armed.as_deref() == Some(point.name()) && kind_matches {
        eprintln!("fault injection: aborting at {}", point.name());
        std::process::abort();
    }
}

/// No-op outside fault-injection builds.
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub const fn point(_point: Point, _kind: OperationKind) {}
