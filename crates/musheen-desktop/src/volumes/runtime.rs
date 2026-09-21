use super::{
    OperationOutcome, OperationUsage, UsageResolution, VolumeAction, VolumeError, VolumeId,
    VolumeModel, VolumeService, VolumeSubscription, VolumeTrigger,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct VolumeUpdate {
    model: VolumeModel,
    warning: Option<VolumeError>,
}

impl VolumeUpdate {
    #[must_use]
    pub const fn model(&self) -> &VolumeModel {
        &self.model
    }

    #[must_use]
    pub const fn warning(&self) -> Option<&VolumeError> {
        self.warning.as_ref()
    }
}

enum RuntimeCommand {
    Refresh(VolumeTrigger),
    Perform {
        id: VolumeId,
        action: VolumeAction,
        usage: UsageResolution,
        secret: Option<Box<str>>,
        reply: mpsc::SyncSender<Result<OperationOutcome, VolumeError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct VolumeRuntime(Arc<RuntimeInner>);

struct RuntimeInner {
    commands: mpsc::SyncSender<RuntimeCommand>,
    model: Arc<RwLock<VolumeModel>>,
    subscribers: Arc<Mutex<Vec<async_channel::Sender<VolumeUpdate>>>>,
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    event_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for VolumeRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VolumeRuntime")
            .finish_non_exhaustive()
    }
}

impl VolumeRuntime {
    #[must_use]
    pub fn inert() -> Self {
        let (commands, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        Self(Arc::new(RuntimeInner {
            commands,
            model: Arc::new(RwLock::new(VolumeModel::default())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(AtomicBool::new(false)),
            worker: Mutex::new(None),
            event_thread: Mutex::new(None),
        }))
    }

    #[must_use]
    pub fn shares_worker_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn system_with_usage(usage: Arc<dyn OperationUsage>) -> Result<Self, VolumeError> {
        Ok(Self::spawn(
            VolumeService::system_with_usage(usage)?,
            Some(VolumeSubscription::system()),
        ))
    }

    #[must_use]
    pub fn from_service(service: VolumeService) -> Self {
        Self::spawn(service, None)
    }

    fn spawn(mut service: VolumeService, events: Option<VolumeSubscription>) -> Self {
        let (commands, receiver) = mpsc::sync_channel(32);
        let model = Arc::new(RwLock::new(service.model().clone()));
        let subscribers = Arc::new(Mutex::new(Vec::<async_channel::Sender<VolumeUpdate>>::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_model = Arc::clone(&model);
        let worker_subscribers = Arc::clone(&subscribers);
        let worker = thread::Builder::new()
            .name("musheen-volume-service".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    let warning = match command {
                        RuntimeCommand::Refresh(trigger) => match service.handle(trigger) {
                            Ok(report) => report.warning().cloned(),
                            Err(error) => Some(error),
                        },
                        RuntimeCommand::Perform {
                            id,
                            action,
                            usage,
                            secret,
                            reply,
                        } => {
                            let result = service.perform(&id, action, usage, secret.as_deref());
                            let warning = result.as_ref().err().cloned();
                            let _ = reply.send(result.clone());
                            warning
                        }
                        RuntimeCommand::Shutdown => break,
                    };
                    let snapshot = service.model().clone();
                    if let Ok(mut current) = worker_model.write() {
                        *current = snapshot.clone();
                    }
                    let update = VolumeUpdate {
                        model: snapshot,
                        warning,
                    };
                    if let Ok(mut listeners) = worker_subscribers.lock() {
                        listeners.retain(|listener| match listener.try_send(update.clone()) {
                            Ok(()) | Err(async_channel::TrySendError::Full(_)) => true,
                            Err(async_channel::TrySendError::Closed(_)) => false,
                        });
                    }
                }
            })
            .expect("volume service worker starts");
        let event_thread = events.map(|events| {
            let commands = commands.clone();
            let stopped = Arc::clone(&stopped);
            thread::Builder::new()
                .name("musheen-volume-dispatch".into())
                .spawn(move || {
                    while !stopped.load(Ordering::Acquire) {
                        if let Some(trigger) = events.recv_timeout(Duration::from_millis(100)) {
                            match commands.try_send(RuntimeCommand::Refresh(trigger)) {
                                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                                Err(mpsc::TrySendError::Disconnected(_)) => break,
                            }
                        }
                    }
                })
                .expect("volume event dispatcher starts")
        });
        Self(Arc::new(RuntimeInner {
            commands,
            model,
            subscribers,
            stopped,
            worker: Mutex::new(Some(worker)),
            event_thread: Mutex::new(event_thread),
        }))
    }

    #[must_use]
    pub fn snapshot(&self) -> VolumeModel {
        self.0
            .model
            .read()
            .map_or_else(|_| VolumeModel::default(), |model| model.clone())
    }

    #[must_use]
    pub fn subscribe(&self) -> async_channel::Receiver<VolumeUpdate> {
        let (sender, receiver) = async_channel::bounded(8);
        let initial = VolumeUpdate {
            model: self.snapshot(),
            warning: None,
        };
        let _ = sender.try_send(initial);
        if let Ok(mut subscribers) = self.0.subscribers.lock() {
            subscribers.push(sender);
        }
        receiver
    }

    pub fn refresh(&self, trigger: VolumeTrigger) -> Result<(), VolumeError> {
        self.0
            .commands
            .send(RuntimeCommand::Refresh(trigger))
            .map_err(|_| VolumeError::Disconnected("the volume worker stopped".into()))
    }

    pub fn perform(
        &self,
        id: VolumeId,
        action: VolumeAction,
        usage: UsageResolution,
        secret: Option<Box<str>>,
    ) -> Result<OperationOutcome, VolumeError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.0
            .commands
            .send(RuntimeCommand::Perform {
                id,
                action,
                usage,
                secret,
                reply,
            })
            .map_err(|_| VolumeError::Disconnected("the volume worker stopped".into()))?;
        result
            .recv()
            .map_err(|_| VolumeError::Disconnected("the volume worker stopped".into()))?
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut event_thread) = self.event_thread.lock()
            && let Some(thread) = event_thread.take()
        {
            let _ = thread.join();
        }
        let _ = self.commands.send(RuntimeCommand::Shutdown);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(thread) = worker.take()
        {
            let _ = thread.join();
        }
    }
}
