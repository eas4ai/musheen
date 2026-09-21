use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    LogRotationTask, MaintenanceCoordinator, MaintenanceError, MaintenanceTask, WindowReadiness,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
