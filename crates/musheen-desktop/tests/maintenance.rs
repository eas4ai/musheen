use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    LogRotationTask, MaintenanceCoordinator, MaintenanceError, MaintenanceTask, WindowReadiness,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct CountingTask(Arc<AtomicUsize>);

impl MaintenanceTask for CountingTask {
    fn run(
        &self,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<(), MaintenanceError>> {
        let calls = Arc::clone(&self.0);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct FailingTask;

impl MaintenanceTask for FailingTask {
    fn run(
        &self,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<(), MaintenanceError>> {
        Box::pin(async { Err(MaintenanceError::Task("fixture failure".into())) })
    }
}

struct BlockingTask(async_channel::Sender<()>);

impl MaintenanceTask for BlockingTask {
    fn run(
        &self,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<(), MaintenanceError>> {
        let started = self.0.clone();
        Box::pin(async move {
            let _ = started.try_send(());
            std::thread::park();
            Ok(())
        })
    }
}

#[test]
fn maintenance_waits_for_first_window_and_does_not_block_readiness() {
    let calls = Arc::new(AtomicUsize::new(0));
    let readiness = WindowReadiness::new();
    let coordinator = MaintenanceCoordinator::new(
        readiness.clone(),
        vec![Arc::new(CountingTask(Arc::clone(&calls)))],
    );
    let worker = coordinator.start();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    readiness.mark_first_window_ready();
    worker.wait().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn task_failures_are_reported_independently_and_do_not_skip_later_tasks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let readiness = WindowReadiness::new();
    let worker = MaintenanceCoordinator::new(
        readiness.clone(),
        vec![
            Arc::new(FailingTask),
            Arc::new(CountingTask(Arc::clone(&calls))),
        ],
    )
    .start();
    readiness.mark_first_window_ready();
    let report = worker.wait().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.failures().len(), 1);
}

#[test]
fn dropping_worker_is_bounded_even_when_a_task_blocks() {
    let readiness = WindowReadiness::new();
    let (started, ready) = async_channel::bounded(1);
    let worker =
        MaintenanceCoordinator::new(readiness.clone(), vec![Arc::new(BlockingTask(started))])
            .start();
    readiness.mark_first_window_ready();
    ready.recv_blocking().unwrap();
    let started = Instant::now();
    drop(worker);
    assert!(started.elapsed() < Duration::from_millis(250));
}

#[test]
fn log_rotation_runs_on_the_maintenance_worker_and_keeps_a_bounded_history() {
    let temporary = tempfile::tempdir().unwrap();
    let log = temporary.path().join("musheen.log");
    std::fs::write(&log, b"current log").unwrap();
    std::fs::write(temporary.path().join("musheen.log.1"), b"older log").unwrap();
    std::fs::write(temporary.path().join("musheen.log.2"), b"oldest log").unwrap();
    let readiness = WindowReadiness::new();
    let coordinator = MaintenanceCoordinator::new(
        readiness.clone(),
        vec![Arc::new(LogRotationTask::new(log.clone(), 2))],
    );
    let worker = coordinator.start();

    readiness.mark_first_window_ready();
    worker.wait().unwrap();

    assert_eq!(
        std::fs::read(temporary.path().join("musheen.log.1")).unwrap(),
        b"current log"
    );
    assert_eq!(
        std::fs::read(temporary.path().join("musheen.log.2")).unwrap(),
        b"older log"
    );
    assert!(!temporary.path().join("musheen.log.3").exists());
}
