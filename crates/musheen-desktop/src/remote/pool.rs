use super::{ConnectionProfile, RemoteError, RemoteErrorCategory};
use musheen_core::{BoxFuture, CancellationToken};
use std::collections::VecDeque;
use std::fmt;
use std::future::{Future, poll_fn};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;
use std::time::{Duration, Instant};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const REQUESTS_PER_CONNECTION: usize = 4;
pub const CONNECTIONS_PER_PROVIDER: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolLimits {
    connect_timeout: Duration,
    idle_timeout: Duration,
    requests_per_connection: usize,
    connections_per_provider: usize,
}

impl PoolLimits {
    pub fn new(
        connect_timeout: Duration,
        idle_timeout: Duration,
        requests_per_connection: usize,
        connections_per_provider: usize,
    ) -> Result<Self, RemoteError> {
        if connect_timeout.is_zero()
            || connect_timeout > CONNECT_TIMEOUT
            || idle_timeout.is_zero()
            || idle_timeout > IDLE_TIMEOUT
            || requests_per_connection == 0
            || requests_per_connection > REQUESTS_PER_CONNECTION
            || connections_per_provider == 0
            || connections_per_provider > CONNECTIONS_PER_PROVIDER
        {
            return Err(RemoteError::new(
                super::RemoteProtocol::Http,
                RemoteErrorCategory::InvalidProfile,
                None,
            ));
        }
        Ok(Self {
            connect_timeout,
            idle_timeout,
            requests_per_connection,
            connections_per_provider,
        })
    }

    #[must_use]
    pub const fn connect_timeout(self) -> Duration {
        self.connect_timeout
    }

    #[must_use]
    pub const fn idle_timeout(self) -> Duration {
        self.idle_timeout
    }

    #[must_use]
    pub const fn requests_per_connection(self) -> usize {
        self.requests_per_connection
    }

    #[must_use]
    pub const fn connections_per_provider(self) -> usize {
        self.connections_per_provider
    }
}

impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            requests_per_connection: REQUESTS_PER_CONNECTION,
            connections_per_provider: CONNECTIONS_PER_PROVIDER,
        }
    }
}

pub trait PoolRuntime: Clone + Send + Sync + 'static {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
}

#[derive(Clone, Debug)]
pub struct SystemPoolRuntime {
    epoch: Instant,
}

impl Default for SystemPoolRuntime {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }
}

impl PoolRuntime for SystemPoolRuntime {
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            async_io::Timer::after(duration).await;
        })
    }
}

pub trait RemoteConnector: Send + Sync + 'static {
    type Connection: Send + Sync + 'static;

    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Self::Connection, RemoteErrorCategory>>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PoolStats {
    connections: usize,
    active_requests: usize,
    waiting_requests: usize,
    connecting: usize,
}

impl PoolStats {
    #[must_use]
    pub const fn connections(self) -> usize {
        self.connections
    }

    #[must_use]
    pub const fn active_requests(self) -> usize {
        self.active_requests
    }

    #[must_use]
    pub const fn waiting_requests(self) -> usize {
        self.waiting_requests
    }

    #[must_use]
    pub const fn connecting(self) -> usize {
        self.connecting
    }
}

pub struct ProviderPool<C: RemoteConnector, R: PoolRuntime = SystemPoolRuntime> {
    inner: Arc<PoolInner<C, R>>,
}

impl<C: RemoteConnector, R: PoolRuntime> Drop for ProviderPool<C, R> {
    fn drop(&mut self) {
        self.inner.maintenance_stop.cancel();
        self.inner.wake_maintenance();
        lock(&self.inner.state).slots.clear();
    }
}

impl<C: RemoteConnector> ProviderPool<C, SystemPoolRuntime> {
    pub fn new(profile: ConnectionProfile, connector: C) -> Result<Self, RemoteError> {
        Self::with_runtime(
            profile,
            connector,
            SystemPoolRuntime::default(),
            PoolLimits::default(),
        )
    }
}

impl<C: RemoteConnector, R: PoolRuntime> ProviderPool<C, R> {
    pub fn with_runtime(
        profile: ConnectionProfile,
        connector: C,
        runtime: R,
        limits: PoolLimits,
    ) -> Result<Self, RemoteError> {
        Self::with_maintenance(
            profile,
            connector,
            runtime,
            limits,
            shared_maintenance_service(),
        )
    }

    fn with_maintenance(
        profile: ConnectionProfile,
        connector: C,
        runtime: R,
        limits: PoolLimits,
        maintenance: Result<Arc<MaintenanceService>, ()>,
    ) -> Result<Self, RemoteError> {
        let maintenance = maintenance.map_err(|()| {
            RemoteError::new(
                profile.protocol(),
                RemoteErrorCategory::Unavailable,
                Some(profile.host().clone()),
            )
        })?;
        let (maintenance_wake, maintenance_events) = async_channel::bounded(1);
        let maintenance_stop = CancellationToken::new();
        let inner = Arc::new(PoolInner {
            profile,
            connector,
            runtime,
            limits,
            state: Mutex::new(PoolState::default()),
            maintenance_wake,
            maintenance_stop: maintenance_stop.clone(),
        });
        let weak = Arc::downgrade(&inner);
        maintenance.schedule(maintain_idle_connections(
            weak,
            maintenance_events,
            maintenance_stop,
        ));
        Ok(Self { inner })
    }

    pub async fn acquire(
        &self,
        cancellation: CancellationToken,
    ) -> Result<PoolLease<C::Connection>, RemoteError> {
        if cancellation.is_cancelled() {
            return Err(self.inner.error(RemoteErrorCategory::Cancelled));
        }
        let (sender, receiver) = async_channel::bounded(1);
        let mut waiter = self.inner.register_waiter(sender);

        loop {
            if cancellation.is_cancelled() {
                return Err(self.inner.error(RemoteErrorCategory::Cancelled));
            }
            match self.inner.claim(&mut waiter) {
                Claim::Lease {
                    slot_id,
                    connection,
                } => return Ok(self.inner.lease(slot_id, connection)),
                Claim::Connect(permit) => {
                    let result = self.inner.connect(permit, cancellation.clone()).await;
                    match result {
                        Ok((slot_id, connection)) => {
                            return Ok(self.inner.lease(slot_id, connection));
                        }
                        Err(error) => return Err(error),
                    }
                }
                Claim::Wait => {
                    futures_lite::future::race(
                        async {
                            let _ = receiver.recv().await;
                        },
                        cancelled(cancellation.clone()),
                    )
                    .await;
                }
            }
        }
    }

    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let state = lock(&self.inner.state);
        PoolStats {
            connections: state.slots.len(),
            active_requests: state.slots.iter().map(|slot| slot.active).sum(),
            waiting_requests: state.waiters.len(),
            connecting: state.connecting,
        }
    }
}

impl<C: RemoteConnector, R: PoolRuntime> Clone for ProviderPool<C, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct PoolInner<C: RemoteConnector, R: PoolRuntime> {
    profile: ConnectionProfile,
    connector: C,
    runtime: R,
    limits: PoolLimits,
    state: Mutex<PoolState<C::Connection>>,
    maintenance_wake: async_channel::Sender<()>,
    maintenance_stop: CancellationToken,
}

impl<C: RemoteConnector, R: PoolRuntime> PoolInner<C, R> {
    fn register_waiter(self: &Arc<Self>, sender: async_channel::Sender<()>) -> WaiterPermit<C, R> {
        let mut state = lock(&self.state);
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        state.waiters.push_back(Waiter { ticket, sender });
        WaiterPermit {
            inner: self.clone(),
            ticket,
            active: true,
        }
    }

    fn claim(self: &Arc<Self>, waiter: &mut WaiterPermit<C, R>) -> Claim<C, R> {
        let now = self.runtime.now();
        let mut state = lock(&self.state);
        state.slots.retain(|slot| {
            slot.active != 0
                || (!slot.discard && now.saturating_sub(slot.last_used) < self.limits.idle_timeout)
        });
        if state.waiters.front().map(|entry| entry.ticket) != Some(waiter.ticket) {
            return Claim::Wait;
        }
        if let Some(slot) = state
            .slots
            .iter_mut()
            .find(|slot| !slot.discard && slot.active < self.limits.requests_per_connection)
        {
            slot.active += 1;
            self.wake_maintenance();
            let claim = Claim::Lease {
                slot_id: slot.id,
                connection: slot.connection.clone(),
            };
            state.waiters.pop_front();
            waiter.disarm();
            wake_front(&mut state);
            return claim;
        }
        if state.slots.len() + state.connecting < self.limits.connections_per_provider {
            state.connecting += 1;
            state.waiters.pop_front();
            waiter.disarm();
            wake_front(&mut state);
            return Claim::Connect(ConnectPermit {
                inner: self.clone(),
                active: true,
            });
        }
        Claim::Wait
    }

    async fn connect(
        &self,
        mut permit: ConnectPermit<C, R>,
        cancellation: CancellationToken,
    ) -> Result<(u64, Arc<C::Connection>), RemoteError> {
        let connector_cancellation = CancellationToken::new();
        let _cancel_connector = CancelOnDrop(connector_cancellation.clone());
        let connect = self
            .connector
            .connect(&self.profile, connector_cancellation.clone());
        let expiry = futures_lite::future::race(
            async {
                cancelled(cancellation).await;
                Err(RemoteErrorCategory::Cancelled)
            },
            async {
                self.runtime.sleep(self.limits.connect_timeout).await;
                Err(RemoteErrorCategory::Timeout)
            },
        );
        let result = futures_lite::future::race(connect, expiry).await;

        let mut state = lock(&self.state);
        state.connecting = state.connecting.saturating_sub(1);
        permit.disarm();
        match result {
            Ok(connection) => {
                let connection = Arc::new(connection);
                let slot_id = state.next_slot;
                state.next_slot = state.next_slot.wrapping_add(1);
                state.slots.push(ConnectionSlot {
                    id: slot_id,
                    connection: connection.clone(),
                    active: 1,
                    last_used: self.runtime.now(),
                    discard: false,
                });
                wake_front(&mut state);
                Ok((slot_id, connection))
            }
            Err(category) => {
                wake_front(&mut state);
                Err(self.error(category))
            }
        }
    }

    fn lease(
        self: &Arc<Self>,
        slot_id: u64,
        connection: Arc<C::Connection>,
    ) -> PoolLease<C::Connection> {
        PoolLease {
            slot_id,
            connection,
            release: self.clone(),
            discard: false,
        }
    }

    fn remove_waiter(&self, ticket: u64) {
        let mut state = lock(&self.state);
        state.waiters.retain(|waiter| waiter.ticket != ticket);
        wake_front(&mut state);
    }

    fn error(&self, category: RemoteErrorCategory) -> RemoteError {
        RemoteError::new(
            self.profile.protocol(),
            category,
            Some(self.profile.host().clone()),
        )
    }

    fn wake_maintenance(&self) {
        let _ = self.maintenance_wake.try_send(());
    }
}

impl<C: RemoteConnector, R: PoolRuntime> Drop for PoolInner<C, R> {
    fn drop(&mut self) {
        self.maintenance_stop.cancel();
        let _ = self.maintenance_wake.try_send(());
    }
}

trait LeaseRelease: Send + Sync {
    fn release(&self, slot_id: u64, discard: bool);
}

impl<C: RemoteConnector, R: PoolRuntime> LeaseRelease for PoolInner<C, R> {
    fn release(&self, slot_id: u64, discard: bool) {
        let now = self.runtime.now();
        let mut state = lock(&self.state);
        if let Some(slot) = state.slots.iter_mut().find(|slot| slot.id == slot_id) {
            slot.active = slot.active.saturating_sub(1);
            slot.last_used = now;
            slot.discard |= discard;
        }
        state
            .slots
            .retain(|slot| !(slot.discard && slot.active == 0));
        wake_front(&mut state);
        drop(state);
        self.wake_maintenance();
    }
}

pub struct PoolLease<T: Send + Sync + 'static> {
    slot_id: u64,
    connection: Arc<T>,
    release: Arc<dyn LeaseRelease>,
    discard: bool,
}

impl<T: Send + Sync + 'static> PoolLease<T> {
    #[must_use]
    pub fn connection(&self) -> &T {
        &self.connection
    }

    /// Prevents a failed connection from being reused after this request ends.
    pub fn discard(mut self) {
        self.discard = true;
    }
}

impl<T: Send + Sync + 'static> fmt::Debug for PoolLease<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PoolLease")
            .field("slot_id", &self.slot_id)
            .field("discard", &self.discard)
            .finish_non_exhaustive()
    }
}

impl<T: Send + Sync + 'static> Drop for PoolLease<T> {
    fn drop(&mut self) {
        self.release.release(self.slot_id, self.discard);
    }
}

struct ConnectionSlot<T> {
    id: u64,
    connection: Arc<T>,
    active: usize,
    last_used: Duration,
    discard: bool,
}

struct Waiter {
    ticket: u64,
    sender: async_channel::Sender<()>,
}

struct PoolState<T> {
    slots: Vec<ConnectionSlot<T>>,
    waiters: VecDeque<Waiter>,
    next_ticket: u64,
    next_slot: u64,
    connecting: usize,
}

impl<T> Default for PoolState<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            waiters: VecDeque::new(),
            next_ticket: 0,
            next_slot: 0,
            connecting: 0,
        }
    }
}

enum Claim<C: RemoteConnector, R: PoolRuntime> {
    Lease {
        slot_id: u64,
        connection: Arc<C::Connection>,
    },
    Connect(ConnectPermit<C, R>),
    Wait,
}

struct WaiterPermit<C: RemoteConnector, R: PoolRuntime> {
    inner: Arc<PoolInner<C, R>>,
    ticket: u64,
    active: bool,
}

impl<C: RemoteConnector, R: PoolRuntime> WaiterPermit<C, R> {
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl<C: RemoteConnector, R: PoolRuntime> Drop for WaiterPermit<C, R> {
    fn drop(&mut self) {
        if self.active {
            self.inner.remove_waiter(self.ticket);
        }
    }
}

struct ConnectPermit<C: RemoteConnector, R: PoolRuntime> {
    inner: Arc<PoolInner<C, R>>,
    active: bool,
}

impl<C: RemoteConnector, R: PoolRuntime> ConnectPermit<C, R> {
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl<C: RemoteConnector, R: PoolRuntime> Drop for ConnectPermit<C, R> {
    fn drop(&mut self) {
        if self.active {
            let mut state = lock(&self.inner.state);
            state.connecting = state.connecting.saturating_sub(1);
            wake_front(&mut state);
        }
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn wake_front<T>(state: &mut PoolState<T>) {
    loop {
        let Some(waiter) = state.waiters.front() else {
            return;
        };
        if waiter.sender.is_closed() {
            state.waiters.pop_front();
            continue;
        }
        let sent = waiter.sender.try_send(());
        if sent.is_err() && waiter.sender.is_closed() {
            state.waiters.pop_front();
            continue;
        }
        return;
    }
}

async fn cancelled(cancellation: CancellationToken) {
    poll_fn(move |context| {
        if cancellation.is_cancelled() {
            Poll::Ready(())
        } else {
            cancellation.register_waker(context.waker());
            Poll::Pending
        }
    })
    .await;
}

async fn maintain_idle_connections<C: RemoteConnector, R: PoolRuntime>(
    weak: Weak<PoolInner<C, R>>,
    events: async_channel::Receiver<()>,
    stop: CancellationToken,
) {
    enum Event {
        Changed,
        Expired,
        Stopped,
    }

    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let next_expiry = {
            let state = lock(&inner.state);
            state
                .slots
                .iter()
                .filter(|slot| slot.active == 0 && !slot.discard)
                .map(|slot| slot.last_used.saturating_add(inner.limits.idle_timeout))
                .min()
        };
        let runtime = inner.runtime.clone();
        drop(inner);

        let event = if let Some(deadline) = next_expiry {
            let delay = deadline.saturating_sub(runtime.now());
            futures_lite::future::race(
                async {
                    cancelled(stop.clone()).await;
                    Event::Stopped
                },
                futures_lite::future::race(
                    async {
                        let _ = events.recv().await;
                        Event::Changed
                    },
                    async {
                        runtime.sleep(delay).await;
                        Event::Expired
                    },
                ),
            )
            .await
        } else {
            futures_lite::future::race(
                async {
                    cancelled(stop.clone()).await;
                    Event::Stopped
                },
                async {
                    let _ = events.recv().await;
                    Event::Changed
                },
            )
            .await
        };

        match event {
            Event::Changed => {}
            Event::Stopped => return,
            Event::Expired => {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let now = inner.runtime.now();
                let mut state = lock(&inner.state);
                state.slots.retain(|slot| {
                    slot.active != 0
                        || (!slot.discard
                            && now.saturating_sub(slot.last_used) < inner.limits.idle_timeout)
                });
                wake_front(&mut state);
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

trait MaintenanceThreadSpawner {
    fn spawn(
        &self,
        run: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<std::thread::JoinHandle<()>>;
}

struct SystemMaintenanceThreadSpawner;

impl MaintenanceThreadSpawner for SystemMaintenanceThreadSpawner {
    fn spawn(
        &self,
        run: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name("musheen-remote-maintenance".to_owned())
            .spawn(run)
    }
}

struct MaintenanceService {
    executor: Arc<async_executor::Executor<'static>>,
    active_tasks: Arc<AtomicUsize>,
    shutdown: async_channel::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl MaintenanceService {
    fn start(spawner: &impl MaintenanceThreadSpawner) -> io::Result<Self> {
        let executor = Arc::new(async_executor::Executor::new());
        let worker_executor = executor.clone();
        let (shutdown, stopped) = async_channel::bounded(1);
        let worker = spawner.spawn(Box::new(move || {
            futures_lite::future::block_on(worker_executor.run(async move {
                let _ = stopped.recv().await;
            }))
        }))?;
        Ok(Self {
            executor,
            active_tasks: Arc::new(AtomicUsize::new(0)),
            shutdown,
            worker: Some(worker),
        })
    }

    fn schedule(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.active_tasks.fetch_add(1, Ordering::AcqRel);
        let active_tasks = self.active_tasks.clone();
        self.executor
            .spawn(async move {
                let _active = ActiveMaintenanceTask(active_tasks);
                future.await;
            })
            .detach();
    }
}

impl Drop for MaintenanceService {
    fn drop(&mut self) {
        let _ = self.shutdown.try_send(());
        if let Some(worker) = self.worker.take()
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
    }
}

struct ActiveMaintenanceTask(Arc<AtomicUsize>);

impl Drop for ActiveMaintenanceTask {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct RetryableShared<T> {
    value: Mutex<Option<Arc<T>>>,
}

impl<T> RetryableShared<T> {
    const fn new() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }

    fn get_or_try_init<E>(&self, initialize: impl FnOnce() -> Result<T, E>) -> Result<Arc<T>, E> {
        let mut value = lock(&self.value);
        if let Some(value) = value.as_ref() {
            return Ok(value.clone());
        }
        let initialized = Arc::new(initialize()?);
        *value = Some(initialized.clone());
        Ok(initialized)
    }
}

struct MaintenanceServiceRegistry {
    service: RetryableShared<MaintenanceService>,
}

impl MaintenanceServiceRegistry {
    const fn new() -> Self {
        Self {
            service: RetryableShared::new(),
        }
    }

    fn get(&self, spawner: &impl MaintenanceThreadSpawner) -> io::Result<Arc<MaintenanceService>> {
        self.service
            .get_or_try_init(|| MaintenanceService::start(spawner))
    }
}

static MAINTENANCE_SERVICE: MaintenanceServiceRegistry = MaintenanceServiceRegistry::new();

fn shared_maintenance_service() -> Result<Arc<MaintenanceService>, ()> {
    MAINTENANCE_SERVICE
        .get(&SystemMaintenanceThreadSpawner)
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{HostKeyPolicy, RemoteHost, RemoteProtocol, SecurityPolicy};
    use crate::{ConnectionId, CredentialReference};

    #[derive(Clone)]
    struct FixedRuntime;

    impl PoolRuntime for FixedRuntime {
        fn now(&self) -> Duration {
            Duration::ZERO
        }

        fn sleep(&self, _: Duration) -> BoxFuture<'_, ()> {
            Box::pin(std::future::pending())
        }
    }

    struct NeverConnector;

    impl RemoteConnector for NeverConnector {
        type Connection = ();

        fn connect<'a>(
            &'a self,
            _: &'a ConnectionProfile,
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<Self::Connection, RemoteErrorCategory>> {
            Box::pin(std::future::pending())
        }
    }

    struct FailingSpawner;

    impl MaintenanceThreadSpawner for FailingSpawner {
        fn spawn(
            &self,
            _: Box<dyn FnOnce() + Send + 'static>,
        ) -> io::Result<std::thread::JoinHandle<()>> {
            Err(io::Error::from(io::ErrorKind::ResourceBusy))
        }
    }

    struct FailOnceSpawner {
        attempts: AtomicUsize,
    }

    impl FailOnceSpawner {
        fn new() -> Self {
            Self {
                attempts: AtomicUsize::new(0),
            }
        }
    }

    impl MaintenanceThreadSpawner for FailOnceSpawner {
        fn spawn(
            &self,
            run: Box<dyn FnOnce() + Send + 'static>,
        ) -> io::Result<std::thread::JoinHandle<()>> {
            if self.attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                Err(io::Error::from(io::ErrorKind::ResourceBusy))
            } else {
                std::thread::Builder::new().spawn(run)
            }
        }
    }

    fn profile() -> ConnectionProfile {
        let protocol = RemoteProtocol::Sftp;
        ConnectionProfile::new(
            ConnectionId::new("pool-test").unwrap(),
            "Pool test",
            protocol,
            RemoteHost::new(protocol, "files.example.test").unwrap(),
            None,
            "/",
            Some("alice"),
            Some(CredentialReference::persistent(
                ConnectionId::new("pool-test-secret").unwrap(),
            )),
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
            None,
        )
        .unwrap()
    }

    #[test]
    fn worker_spawn_failure_is_typed_and_never_panics() {
        let spawn_error = match MaintenanceService::start(&FailingSpawner) {
            Ok(_) => panic!("failing spawner must not create a service"),
            Err(error) => error,
        };
        assert_eq!(spawn_error.kind(), io::ErrorKind::ResourceBusy);
        let result = ProviderPool::with_maintenance(
            profile(),
            NeverConnector,
            FixedRuntime,
            PoolLimits::default(),
            Err(()),
        );
        let error = match result {
            Ok(_) => panic!("maintenance failure must reject pool construction"),
            Err(error) => error,
        };
        assert_eq!(error.category(), RemoteErrorCategory::Unavailable);
    }

    #[test]
    fn shared_maintenance_initialization_retries_after_a_transient_failure() {
        let registry = MaintenanceServiceRegistry::new();
        let spawner = FailOnceSpawner::new();

        let first_error = match registry.get(&spawner) {
            Ok(_) => panic!("first maintenance worker spawn must fail"),
            Err(error) => error,
        };
        assert_eq!(first_error.kind(), io::ErrorKind::ResourceBusy);

        let recovered = registry.get(&spawner).unwrap();
        let reused = registry.get(&spawner).unwrap();
        assert!(Arc::ptr_eq(&recovered, &reused));
        assert_eq!(spawner.attempts.load(Ordering::Acquire), 2);
    }

    #[test]
    fn concurrent_maintenance_retries_create_exactly_one_shared_worker() {
        let registry = Arc::new(MaintenanceServiceRegistry::new());
        let spawner = Arc::new(FailOnceSpawner::new());
        let first_error = match registry.get(spawner.as_ref()) {
            Ok(_) => panic!("first maintenance worker spawn must fail"),
            Err(error) => error,
        };
        assert_eq!(first_error.kind(), io::ErrorKind::ResourceBusy);

        let barrier = Arc::new(std::sync::Barrier::new(16));
        let mut retries = Vec::new();
        for _ in 0..16 {
            let registry = registry.clone();
            let spawner = spawner.clone();
            let barrier = barrier.clone();
            retries.push(std::thread::spawn(move || {
                barrier.wait();
                registry.get(spawner.as_ref()).unwrap()
            }));
        }

        let services = retries
            .into_iter()
            .map(|retry| retry.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(spawner.attempts.load(Ordering::Acquire), 2);
        assert!(
            services
                .iter()
                .all(|service| Arc::ptr_eq(service, &services[0]))
        );
    }

    #[test]
    fn dropping_pool_cancels_and_deregisters_maintenance() {
        let service = Arc::new(MaintenanceService::start(&SystemMaintenanceThreadSpawner).unwrap());
        let pool = ProviderPool::with_maintenance(
            profile(),
            NeverConnector,
            FixedRuntime,
            PoolLimits::default(),
            Ok(service.clone()),
        )
        .unwrap();
        assert_eq!(service.active_tasks.load(Ordering::Acquire), 1);
        drop(pool);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if service.active_tasks.load(Ordering::Acquire) == 0 {
                return;
            }
            std::thread::yield_now();
        }
        panic!("maintenance task did not deregister after pool drop");
    }
}
