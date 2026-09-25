use musheen_core::{BoxFuture, CancellationToken};
use std::error::Error;
use std::fmt;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::{fs, io};

const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Clone, Default)]
pub struct WindowReadiness(Arc<(Mutex<bool>, Condvar)>);

impl WindowReadiness {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_first_window_ready(&self) {
        let (lock, ready) = &*self.0;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        ready.notify_all();
    }

    fn wait(&self, cancellation: &CancellationToken) -> Result<(), MaintenanceError> {
        let (lock, ready) = &*self.0;
        let mut state = lock.lock().map_err(|_| MaintenanceError::Stopped)?;
        while !*state && !cancellation.is_cancelled() {
            let (next, _) = ready
                .wait_timeout(state, std::time::Duration::from_millis(50))
                .map_err(|_| MaintenanceError::Stopped)?;
            state = next;
        }
        if cancellation.is_cancelled() {
            Err(MaintenanceError::Cancelled)
        } else {
            Ok(())
        }
    }
}

pub trait MaintenanceTask: Send + Sync + 'static {
    fn run(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<(), MaintenanceError>>;
}

pub struct MaintenanceCoordinator {
    readiness: WindowReadiness,
    tasks: Vec<Arc<dyn MaintenanceTask>>,
}

impl MaintenanceCoordinator {
    #[must_use]
    pub fn new(readiness: WindowReadiness, tasks: Vec<Arc<dyn MaintenanceTask>>) -> Self {
        Self { readiness, tasks }
    }

    #[must_use]
    pub fn start(self) -> MaintenanceWorker {
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (completed, completion) = std::sync::mpsc::sync_channel(1);
        let (failure_sender, failure_receiver) = async_channel::bounded(self.tasks.len().max(1));
        let join = std::thread::Builder::new()
            .name("musheen-maintenance".into())
            .spawn(move || {
                let report = match self.readiness.wait(&worker_cancellation) {
                    Ok(()) => {
                        let mut failures = Vec::new();
                        for (index, task) in self.tasks.into_iter().enumerate() {
                            if worker_cancellation.is_cancelled() {
                                break;
                            }
                            if let Err(error) = futures_lite::future::block_on(
                                task.run(worker_cancellation.clone()),
                            ) && error != MaintenanceError::Cancelled
                            {
                                let failure = MaintenanceFailure { index, error };
                                let _ = failure_sender.try_send(failure.clone());
                                failures.push(failure);
                            }
                        }
                        Ok(MaintenanceReport { failures })
                    }
                    Err(error) => Err(error),
                };
                let _ = completed.send(());
                report
            })
            .expect("the operating system must create the maintenance worker");
        MaintenanceWorker {
            cancellation,
            join: Some(join),
            completion,
            failure_receiver,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceFailure {
    index: usize,
    error: MaintenanceError,
}

impl MaintenanceFailure {
    #[must_use]
    pub const fn task_index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub const fn error(&self) -> &MaintenanceError {
        &self.error
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MaintenanceReport {
    failures: Vec<MaintenanceFailure>,
}

impl MaintenanceReport {
    #[must_use]
    pub fn failures(&self) -> &[MaintenanceFailure] {
        &self.failures
    }
}

pub struct MaintenanceWorker {
    cancellation: CancellationToken,
    join: Option<JoinHandle<Result<MaintenanceReport, MaintenanceError>>>,
    completion: Receiver<()>,
    failure_receiver: async_channel::Receiver<MaintenanceFailure>,
}

impl MaintenanceWorker {
    #[must_use]
    pub fn failures(&self) -> async_channel::Receiver<MaintenanceFailure> {
        self.failure_receiver.clone()
    }

    pub fn wait(mut self) -> Result<MaintenanceReport, MaintenanceError> {
        self.join
            .take()
            .ok_or(MaintenanceError::Stopped)?
            .join()
            .map_err(|_| MaintenanceError::Stopped)?
    }
}

impl Drop for MaintenanceWorker {
    fn drop(&mut self) {
        self.cancellation.cancel();
        match self.completion.recv_timeout(SHUTDOWN_GRACE) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(join) = self.join.take() {
                    let _ = join.join();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // A blocking filesystem or HTTP provider cannot delay application
                // teardown. Dropping the handle safely detaches the bounded worker.
                self.join.take();
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaintenanceError {
    Cancelled,
    Stopped,
    Task(Box<str>),
}

impl fmt::Display for MaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("maintenance was cancelled"),
            Self::Stopped => formatter.write_str("the maintenance worker stopped"),
            Self::Task(reason) => write!(formatter, "maintenance failed: {reason}"),
        }
    }
}

impl Error for MaintenanceError {}

#[derive(Clone, Debug)]
pub struct LogRotationTask {
    active_log: std::path::PathBuf,
    keep: usize,
}

impl LogRotationTask {
    #[must_use]
    pub const fn new(active_log: std::path::PathBuf, keep: usize) -> Self {
        Self { active_log, keep }
    }

    #[must_use]
    pub fn for_current_user(keep: usize) -> Self {
        let state_home = std::env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| std::path::PathBuf::from(home).join(".local/state"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        Self::new(state_home.join("musheen/musheen.log"), keep)
    }

    fn rotate(&self) -> io::Result<()> {
        if self.keep == 0 {
            return remove_if_exists(&self.active_log);
        }
        remove_if_exists(&rotated_path(&self.active_log, self.keep))?;
        for index in (1..self.keep).rev() {
            let source = rotated_path(&self.active_log, index);
            if source.exists() {
                fs::rename(source, rotated_path(&self.active_log, index + 1))?;
            }
        }
        if self.active_log.exists() {
            fs::rename(&self.active_log, rotated_path(&self.active_log, 1))?;
        }
        Ok(())
    }
}

impl MaintenanceTask for LogRotationTask {
    fn run(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<(), MaintenanceError>> {
        let task = self.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(MaintenanceError::Cancelled);
            }
            task.rotate()
                .map_err(|error| MaintenanceError::Task(error.to_string().into()))
        })
    }
}

fn rotated_path(active: &std::path::Path, index: usize) -> std::path::PathBuf {
    let mut path = active.as_os_str().to_os_string();
    path.push(format!(".{index}"));
    path.into()
}

fn remove_if_exists(path: &std::path::Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
