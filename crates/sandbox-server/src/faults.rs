//! Crash points for the recovery drills. Compiled in only with the `fault-injection`
//! feature; otherwise every point is an empty inline function. With the feature, setting
//! `PS_FAULT=<point>` aborts the process at that point — no destructors, no flush, no
//! graceful anything, which is what a power cut or OOM kill looks like.

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

/// Aborts the process when `PS_FAULT` names `point` (fault-injection builds only).
#[cfg(feature = "fault-injection")]
pub fn point(point: Point) {
    use std::sync::OnceLock;
    static FAULT: OnceLock<Option<String>> = OnceLock::new();
    let armed = FAULT.get_or_init(|| std::env::var("PS_FAULT").ok());
    if armed.as_deref() == Some(point.name()) {
        eprintln!("fault injection: aborting at {}", point.name());
        std::process::abort();
    }
}

/// No-op outside fault-injection builds.
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub const fn point(_point: Point) {}
