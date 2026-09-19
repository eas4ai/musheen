use crate::StoreError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;

/// A cheap, clonable cancellation signal checked at provider work boundaries.
#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    waiters: Mutex<Vec<Waker>>,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<CancellationState>);

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut waiters = self
            .0
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for waiter in waiters.drain(..) {
            waiter.wake();
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), StoreError> {
        if self.is_cancelled() {
            Err(StoreError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Registers a pending operation to be polled when cancellation occurs.
    pub fn register_waker(&self, waker: &Waker) {
        if self.is_cancelled() {
            waker.wake_by_ref();
            return;
        }

        let mut waiters = self
            .0
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_cancelled() {
            waker.wake_by_ref();
        } else if !waiters.iter().any(|waiter| waiter.will_wake(waker)) {
            waiters.push(waker.clone());
        }
    }
}
