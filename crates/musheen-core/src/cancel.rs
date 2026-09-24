use crate::StoreError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::Waker;

/// A cheap, clonable cancellation signal checked at provider work boundaries.
#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    paused: AtomicBool,
    pause_lock: Mutex<()>,
    pause_waiters: Condvar,
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
        {
            let _guard = self
                .0
                .pause_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.0.cancelled.swap(true, Ordering::AcqRel) {
                return;
            }
            self.0.pause_waiters.notify_all();
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

    pub fn pause(&self) {
        let _guard = self
            .0
            .pause_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.is_cancelled() {
            self.0.paused.store(true, Ordering::Release);
        }
    }

    pub fn resume(&self) {
        let _guard = self
            .0
            .pause_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.0.paused.swap(false, Ordering::AcqRel) {
            self.0.pause_waiters.notify_all();
        }
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.0.paused.load(Ordering::Acquire)
    }

    /// Blocks a provider worker at a safe boundary while this token is paused.
    pub fn wait_if_paused(&self) -> Result<(), StoreError> {
        let mut guard = self
            .0
            .pause_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while self.is_paused() && !self.is_cancelled() {
            guard = self
                .0
                .pause_waiters
                .wait(guard)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        self.check()
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

#[cfg(test)]
mod tests {
    use super::CancellationToken;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn cancel_serializes_with_a_paused_worker_before_notifying() {
        let token = CancellationToken::new();
        token.pause();
        let pause_guard = token.0.pause_lock.lock().unwrap();
        let worker_token = token.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_token.cancel();
            finished_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let finished_while_locked = finished_rx.recv_timeout(Duration::from_millis(200)).is_ok();
        drop(pause_guard);
        worker.join().unwrap();

        assert!(!finished_while_locked);
        assert!(token.is_cancelled());
    }

    #[test]
    fn resume_serializes_with_a_paused_worker_before_notifying() {
        let token = CancellationToken::new();
        token.pause();
        let pause_guard = token.0.pause_lock.lock().unwrap();
        let worker_token = token.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_token.resume();
            finished_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let finished_while_locked = finished_rx.recv_timeout(Duration::from_millis(200)).is_ok();
        drop(pause_guard);
        worker.join().unwrap();

        assert!(!finished_while_locked);
        assert!(!token.is_paused());
    }
}
