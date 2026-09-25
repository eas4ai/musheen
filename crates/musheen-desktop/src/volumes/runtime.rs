use super::{
    OperationOutcome, OperationUsage, UDisksRequest, UsageResolution, VolumeAction, VolumeError,
    VolumeId, VolumeModel, VolumeService, VolumeSubscription, VolumeTrigger,
};
use crate::SecretBuffer;
use musheen_core::CancellationToken;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak, mpsc};
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
        secret: Option<SecretBuffer>,
        request: UDisksRequest,
        reply: mpsc::SyncSender<Result<OperationOutcome, VolumeError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct VolumeRuntime(Arc<RuntimeInner>);

struct RuntimeInner {
    commands: mpsc::SyncSender<RuntimeCommand>,
    model: Arc<RwLock<VolumeModel>>,
    subscribers: Arc<Mutex<Vec<LatestSubscriber>>>,
    next_subscriber: AtomicU64,
    refresh_pending: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    event_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

struct LatestSubscriber {
    id: u64,
    notify: async_channel::Sender<()>,
    latest: Arc<Mutex<VolumeUpdate>>,
}

pub struct VolumeUpdates {
    id: u64,
    notify: async_channel::Receiver<()>,
    latest: Arc<Mutex<VolumeUpdate>>,
    subscribers: Weak<Mutex<Vec<LatestSubscriber>>>,
}

impl VolumeUpdates {
    pub async fn recv(&self) -> Result<VolumeUpdate, async_channel::RecvError> {
        self.notify.recv().await?;
        Ok(self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    pub fn recv_blocking(&self) -> Result<VolumeUpdate, async_channel::RecvError> {
        self.notify.recv_blocking()?;
        Ok(self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    pub fn try_recv(&self) -> Result<VolumeUpdate, async_channel::TryRecvError> {
        self.notify.try_recv()?;
        Ok(self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }
}

impl Drop for VolumeUpdates {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.upgrade()
            && let Ok(mut subscribers) = subscribers.lock()
        {
            subscribers.retain(|subscriber| subscriber.id != self.id);
        }
        self.notify.close();
    }
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
            next_subscriber: AtomicU64::new(1),
            refresh_pending: Arc::new(AtomicBool::new(false)),
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

    #[must_use]
    pub fn from_service_with_subscription(
        service: VolumeService,
        subscription: VolumeSubscription,
    ) -> Self {
        Self::spawn(service, Some(subscription))
    }

    fn spawn(mut service: VolumeService, events: Option<VolumeSubscription>) -> Self {
        // One pending command is enough: event refreshes coalesce and shutdown
        // is an out-of-band cancellation flag, never the tail of a FIFO.
        let (commands, receiver) = mpsc::sync_channel(1);
        let model = Arc::new(RwLock::new(service.model().clone()));
        let subscribers = Arc::new(Mutex::new(Vec::<LatestSubscriber>::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_model = Arc::clone(&model);
        let worker_subscribers = Arc::clone(&subscribers);
        let worker_stopped = Arc::clone(&stopped);
        let refresh_pending = Arc::new(AtomicBool::new(false));
        let worker_refresh_pending = Arc::clone(&refresh_pending);
        let worker = thread::Builder::new()
            .name("musheen-volume-service".into())
            .spawn(move || {
                while !worker_stopped.load(Ordering::Acquire) {
                    let Some(command) = next_command(&receiver, &worker_refresh_pending) else {
                        break;
                    };
                    if worker_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let warning = match command {
                        RuntimeCommand::Refresh(trigger) => {
                            let request = UDisksRequest::with_cancel(
                                Duration::from_secs(2),
                                Arc::clone(&worker_stopped),
                            );
                            match service.handle_with_request(trigger, &request) {
                                Ok(report) => report.warning().cloned(),
                                Err(error) => Some(error),
                            }
                        }
                        RuntimeCommand::Perform {
                            id,
                            action,
                            usage,
                            secret,
                            request,
                            reply,
                        } => {
                            let result = perform_volume_request(
                                &mut service,
                                &id,
                                action,
                                usage,
                                secret.as_ref(),
                                &request,
                            );
                            let warning = match &result {
                                Ok(outcome) => outcome.refresh().warning().cloned(),
                                Err(error) => Some(error.clone()),
                            };
                            let _ = reply.send(result.clone());
                            warning
                        }
                        RuntimeCommand::Shutdown => break,
                    };
                    let snapshot = service.model().clone();
                    let update = VolumeUpdate {
                        model: snapshot,
                        warning,
                    };
                    publish_update(&worker_model, &worker_subscribers, update);
                }
            })
            .expect("volume service worker starts");
        let event_thread = events.map(|events| {
            let commands = commands.clone();
            let stopped = Arc::clone(&stopped);
            let refresh_pending = Arc::clone(&refresh_pending);
            thread::Builder::new()
                .name("musheen-volume-dispatch".into())
                .spawn(move || {
                    while !stopped.load(Ordering::Acquire) {
                        if let Some(trigger) = events.recv_timeout(Duration::from_millis(100)) {
                            match commands.try_send(RuntimeCommand::Refresh(trigger)) {
                                Ok(()) => {}
                                Err(mpsc::TrySendError::Full(_)) => {
                                    refresh_pending.store(true, Ordering::Release);
                                }
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
            next_subscriber: AtomicU64::new(1),
            refresh_pending,
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
    pub fn subscribe(&self) -> VolumeUpdates {
        self.subscribe_with_registration_hook(|| {})
    }

    #[doc(hidden)]
    #[must_use]
    pub fn subscribe_with_registration_hook(&self, hook: impl FnOnce()) -> VolumeUpdates {
        let (notify, receiver) = async_channel::bounded(1);
        let id = self.0.next_subscriber.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut subscribers) = self.0.subscribers.lock() {
            hook();
            let latest = Arc::new(Mutex::new(VolumeUpdate {
                model: self.snapshot(),
                warning: None,
            }));
            let _ = notify.try_send(());
            subscribers.push(LatestSubscriber {
                id,
                notify,
                latest: Arc::clone(&latest),
            });
            return VolumeUpdates {
                id,
                notify: receiver,
                latest,
                subscribers: Arc::downgrade(&self.0.subscribers),
            };
        }
        VolumeUpdates {
            id,
            notify: receiver,
            latest: Arc::new(Mutex::new(VolumeUpdate {
                model: VolumeModel::default(),
                warning: Some(VolumeError::WorkerStopped),
            })),
            subscribers: Weak::new(),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.0.subscribers.lock().map_or(0, |items| items.len())
    }

    pub fn refresh(&self, trigger: VolumeTrigger) -> Result<(), VolumeError> {
        match self.0.commands.try_send(RuntimeCommand::Refresh(trigger)) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                self.0.refresh_pending.store(true, Ordering::Release);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err(VolumeError::WorkerStopped),
        }
    }

    pub fn perform(
        &self,
        id: VolumeId,
        action: VolumeAction,
        usage: UsageResolution,
        secret: Option<SecretBuffer>,
    ) -> Result<OperationOutcome, VolumeError> {
        self.perform_cancellable(id, action, usage, secret, CancellationToken::new())
    }

    pub fn perform_cancellable(
        &self,
        id: VolumeId,
        action: VolumeAction,
        usage: UsageResolution,
        secret: Option<SecretBuffer>,
        cancellation: CancellationToken,
    ) -> Result<OperationOutcome, VolumeError> {
        let (reply, result) = mpsc::sync_channel(1);
        let request = UDisksRequest::with_cancellation(
            action.request_timeout(),
            Arc::clone(&self.0.stopped),
            cancellation,
        );
        let deadline = std::time::Instant::now() + request.remaining();
        let mut command = RuntimeCommand::Perform {
            id,
            action,
            usage,
            secret,
            request: request.clone(),
            reply,
        };
        loop {
            request.check()?;
            match self.0.commands.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(VolumeError::DeadlineExceeded);
                    }
                    command = returned;
                    thread::yield_now();
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(VolumeError::WorkerStopped);
                }
            }
        }
        result
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => VolumeError::DeadlineExceeded,
                mpsc::RecvTimeoutError::Disconnected => VolumeError::WorkerStopped,
            })?
    }
}

fn perform_volume_request(
    service: &mut VolumeService,
    id: &VolumeId,
    action: VolumeAction,
    usage: UsageResolution,
    secret: Option<&SecretBuffer>,
    request: &UDisksRequest,
) -> Result<OperationOutcome, VolumeError> {
    let Some(secret) = secret else {
        return service.perform_with_request(id, action, usage, None, request);
    };
    secret.expose_secret(|bytes| {
        let secret = std::str::from_utf8(bytes)
            .map_err(|_| VolumeError::Protocol("unlock secret is not valid UTF-8".into()))?;
        service.perform_with_request(id, action, usage, Some(secret), request)
    })
}

fn next_command(
    receiver: &mpsc::Receiver<RuntimeCommand>,
    refresh_pending: &AtomicBool,
) -> Option<RuntimeCommand> {
    if let Ok(command) = receiver.try_recv() {
        Some(command)
    } else if refresh_pending.swap(false, Ordering::AcqRel) {
        Some(RuntimeCommand::Refresh(VolumeTrigger::UDisksChanged))
    } else {
        receiver.recv().ok()
    }
}

fn publish_update(
    model: &RwLock<VolumeModel>,
    subscribers: &Mutex<Vec<LatestSubscriber>>,
    update: VolumeUpdate,
) {
    if let Ok(mut listeners) = subscribers.lock() {
        if let Ok(mut current) = model.write() {
            *current = update.model.clone();
        }
        listeners.retain(|listener| {
            *listener.latest.lock().unwrap_or_else(|e| e.into_inner()) = update.clone();
            !matches!(
                listener.notify.try_send(()),
                Err(async_channel::TrySendError::Closed(_))
            )
        });
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
        // Cancellation preempts an in-flight request. A nonblocking wake-up is
        // sufficient when the worker is idle and never waits behind refreshes.
        let _ = self.commands.try_send(RuntimeCommand::Shutdown);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(thread) = worker.take()
        {
            let _ = thread.join();
        }
    }
}
