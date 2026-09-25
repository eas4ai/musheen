use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FaultPhase {
    BeforeWork,
    DuringPartialWork,
    AfterPublication,
    Recovery,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FaultCase {
    pub boundary: &'static str,
    pub phase: FaultPhase,
}

impl FaultCase {
    #[must_use]
    pub const fn new(boundary: &'static str, phase: FaultPhase) -> Self {
        Self { boundary, phase }
    }
}

/// Tracks evidence attached to each declared boundary and failure phase.
#[derive(Debug)]
pub struct FaultCoverage {
    required: BTreeSet<FaultCase>,
    covered: BTreeSet<FaultCase>,
}

impl FaultCoverage {
    #[must_use]
    pub fn new(required: impl IntoIterator<Item = FaultCase>) -> Self {
        Self {
            required: required.into_iter().collect(),
            covered: BTreeSet::new(),
        }
    }

    pub fn cover(&mut self, case: FaultCase) {
        assert!(
            self.required.contains(&case),
            "undeclared fault boundary: {case:?}"
        );
        self.covered.insert(case);
    }

    #[must_use]
    pub fn uncovered(&self) -> Vec<FaultCase> {
        self.required.difference(&self.covered).copied().collect()
    }
}

/// Injects one failure on a zero-based call number, then allows retries.
#[derive(Debug)]
pub struct FaultGate {
    fail_on_call: usize,
    calls: AtomicUsize,
}

impl FaultGate {
    #[must_use]
    pub const fn new(fail_on_call: usize) -> Self {
        Self {
            fail_on_call,
            calls: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn trip(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) == self.fail_on_call
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
