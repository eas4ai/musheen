use musheen_core::{CancellationToken, StoreError};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

#[test]
fn pause_blocks_work_boundaries_until_resume_and_cancel_wakes_waiters() {
    let token = CancellationToken::new();
    token.pause();
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker_token = token.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        sender.send(worker_token.wait_if_paused()).unwrap();
    });
    barrier.wait();
    assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
    token.resume();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(())
    );
    worker.join().unwrap();

    token.pause();
    let cancelled = token.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || sender.send(cancelled.wait_if_paused()).unwrap());
    token.cancel();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(StoreError::Cancelled)
    );
    worker.join().unwrap();
}
