use super::{ConnectionProfile, RemoteError, RemoteErrorCategory};
use musheen_core::{BoxFuture, CancellationToken};
use std::collections::VecDeque;
use std::fmt;
use std::future::poll_fn;
use std::sync::{Arc, Mutex};
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
            || idle_timeout.is_zero()
            || requests_per_connection == 0
            || connections_per_provider == 0
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

impl<C: RemoteConnector> ProviderPool<C, SystemPoolRuntime> {
    #[must_use]
    pub fn new(profile: ConnectionProfile, connector: C) -> Self {
        Self::with_runtime(
            profile,
            connector,
            SystemPoolRuntime::default(),
            PoolLimits::default(),
        )
    }
}

impl<C: RemoteConnector, R: PoolRuntime> ProviderPool<C, R> {
    #[must_use]
    pub fn with_runtime(
        profile: ConnectionProfile,
        connector: C,
        runtime: R,
        limits: PoolLimits,
    ) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                profile,
                connector,
                runtime,
                limits,
                state: Mutex::new(PoolState::default()),
            }),
        }
    }

    pub async fn acquire(
        &self,
        cancellation: CancellationToken,
    ) -> Result<PoolLease<C::Connection>, RemoteError> {
        if cancellation.is_cancelled() {
            return Err(self.inner.error(RemoteErrorCategory::Cancelled));
        }
        let (sender, receiver) = async_channel::bounded(1);
        let ticket = {
            let mut state = lock(&self.inner.state);
            let ticket = state.next_ticket;
            state.next_ticket = state.next_ticket.wrapping_add(1);
            state.waiters.push_back(Waiter { ticket, sender });
            ticket
        };

        loop {
            if cancellation.is_cancelled() {
                self.inner.remove_waiter(ticket);
                return Err(self.inner.error(RemoteErrorCategory::Cancelled));
            }
            match self.inner.claim(ticket) {
                Claim::Lease {
                    slot_id,
                    connection,
                } => return Ok(self.inner.lease(slot_id, connection)),
                Claim::Connect => {
                    let result = self.inner.connect(cancellation.clone()).await;
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
}

impl<C: RemoteConnector, R: PoolRuntime> PoolInner<C, R> {
    fn claim(&self, ticket: u64) -> Claim<C::Connection> {
        let now = self.runtime.now();
        let mut state = lock(&self.state);
        state.slots.retain(|slot| {
            slot.active != 0
                || (!slot.discard && now.saturating_sub(slot.last_used) < self.limits.idle_timeout)
        });
        if state.waiters.front().map(|waiter| waiter.ticket) != Some(ticket) {
            return Claim::Wait;
        }
        if let Some(slot) = state
            .slots
            .iter_mut()
            .find(|slot| !slot.discard && slot.active < self.limits.requests_per_connection)
        {
            slot.active += 1;
            let claim = Claim::Lease {
                slot_id: slot.id,
                connection: slot.connection.clone(),
            };
            state.waiters.pop_front();
            wake_front(&state);
            return claim;
        }
        if state.slots.len() + state.connecting < self.limits.connections_per_provider {
            state.connecting += 1;
            state.waiters.pop_front();
            wake_front(&state);
            return Claim::Connect;
        }
        Claim::Wait
    }

    async fn connect(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(u64, Arc<C::Connection>), RemoteError> {
        let connector_cancellation = CancellationToken::new();
        let expiry_cancellation = connector_cancellation.clone();
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
        expiry_cancellation.cancel();

        let mut state = lock(&self.state);
        state.connecting = state.connecting.saturating_sub(1);
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
                wake_front(&state);
                Ok((slot_id, connection))
            }
            Err(category) => {
                wake_front(&state);
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
        wake_front(&state);
    }

    fn error(&self, category: RemoteErrorCategory) -> RemoteError {
        RemoteError::new(
            self.profile.protocol(),
            category,
            Some(self.profile.host().clone()),
        )
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
        wake_front(&state);
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

enum Claim<T> {
    Lease { slot_id: u64, connection: Arc<T> },
    Connect,
    Wait,
}

fn wake_front<T>(state: &PoolState<T>) {
    if let Some(waiter) = state.waiters.front() {
        let _ = waiter.sender.try_send(());
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

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
