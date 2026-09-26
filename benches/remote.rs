#[path = "support/record.rs"]
mod benchmark_record;
mod support;

use benchmark_record::record;
use futures_lite::future::block_on;
use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::ConnectionId;
use musheen_desktop::remote::{
    ConnectionProfile, HostKeyPolicy, ProviderPool, RemoteConnector, RemoteErrorCategory,
    RemoteHost, RemoteProtocol, SecurityPolicy,
};
use serde_json::json;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use support::Sample;

const SATURATED_REQUESTS: usize = 32;
const EXPECTED_CONNECTIONS: usize = 8;
const ROUNDS: usize = 100;
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Default)]
struct InMemoryConnector {
    calls: Arc<AtomicUsize>,
}

impl InMemoryConnector {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl RemoteConnector for InMemoryConnector {
    type Connection = usize;

    fn connect<'a>(
        &'a self,
        _profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Self::Connection, RemoteErrorCategory>> {
        Box::pin(async move { Ok(self.calls.fetch_add(1, Ordering::AcqRel) + 1) })
    }
}

fn profile() -> BenchResult<ConnectionProfile> {
    let protocol = RemoteProtocol::Sftp;
    Ok(ConnectionProfile::new(
        ConnectionId::new("remote-benchmark")?,
        "Remote benchmark",
        protocol,
        RemoteHost::new(protocol, "benchmark.example.test")?,
        None,
        "/share",
        None::<&str>,
        None,
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        None,
    )?)
}

fn capacity_and_reuse() -> BenchResult<()> {
    let connector = InMemoryConnector::default();
    let before = Sample::capture("musheen-remote-")?;
    let pool = ProviderPool::new(profile()?, connector.clone())?;
    let mut connections_max = 0;
    let mut active_max = 0;
    for _ in 0..ROUNDS {
        let mut leases = Vec::with_capacity(SATURATED_REQUESTS);
        for _ in 0..SATURATED_REQUESTS {
            leases.push(block_on(pool.acquire(CancellationToken::new()))?);
        }
        let stats = pool.stats();
        connections_max = connections_max.max(stats.connections());
        active_max = active_max.max(stats.active_requests());
        drop(leases);
    }
    let after = Sample::capture("musheen-remote-")?;
    if connections_max != EXPECTED_CONNECTIONS
        || active_max != SATURATED_REQUESTS
        || connector.calls() != EXPECTED_CONNECTIONS
    {
        return Err(io::Error::other(format!(
            "remote pool failed to cap or reuse connections: connections={connections_max}, active={active_max}, connects={}",
            connector.calls()
        ))
        .into());
    }
    record(
        "remote_pool_capacity_and_reuse",
        &before,
        &after,
        json!({
            "acquisitions": ROUNDS * SATURATED_REQUESTS,
            "pool_connections_max": connections_max,
            "active_requests_max": active_max,
            "connector_calls": connector.calls(),
            "queued_work_max": 0,
            "retained_models_max": 0,
        }),
    );
    Ok(())
}

fn waiter_backpressure() -> BenchResult<()> {
    let connector = InMemoryConnector::default();
    let before = Sample::capture("musheen-remote-")?;
    let pool = Arc::new(ProviderPool::new(profile()?, connector)?);
    let mut held = Vec::with_capacity(SATURATED_REQUESTS);
    for _ in 0..SATURATED_REQUESTS {
        held.push(block_on(pool.acquire(CancellationToken::new()))?);
    }
    let saturated_stats = pool.stats();
    if saturated_stats.connections() != EXPECTED_CONNECTIONS
        || saturated_stats.active_requests() != SATURATED_REQUESTS
    {
        return Err(io::Error::other("remote pool did not reach its documented capacity").into());
    }
    let cancellation = CancellationToken::new();
    let (sender, receiver) = mpsc::sync_channel(1);
    let waiting_pool = Arc::clone(&pool);
    let waiting_cancellation = cancellation.clone();
    let waiter = std::thread::spawn(move || {
        let result =
            block_on(waiting_pool.acquire(waiting_cancellation)).map(|lease| *lease.connection());
        sender
            .send(result)
            .expect("benchmark receiver remains active");
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.stats().waiting_requests() != 1 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    if pool.stats().waiting_requests() != 1 {
        cancellation.cancel();
        return Err(io::Error::other("remote pool waiter did not queue").into());
    }
    drop(held.pop());
    let awakened_connection = match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => result?,
        Err(error) => {
            cancellation.cancel();
            return Err(error.into());
        }
    };
    waiter
        .join()
        .map_err(|_| io::Error::other("remote pool waiter panicked"))?;
    let after = Sample::capture("musheen-remote-")?;
    if awakened_connection > EXPECTED_CONNECTIONS || pool.stats().waiting_requests() != 0 {
        return Err(io::Error::other("remote pool waiter did not reuse released capacity").into());
    }
    record(
        "remote_pool_waiter_backpressure",
        &before,
        &after,
        json!({
            "waiting_requests_max": 1,
            "awakened": true,
            "pool_connections_max": saturated_stats.connections(),
            "active_requests_max": saturated_stats.active_requests(),
            "queued_work_max": 1,
            "retained_models_max": 0,
        }),
    );
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("remote benchmark skipped; run with cargo bench --bench remote");
        return Ok(());
    }
    capacity_and_reuse()?;
    waiter_backpressure()
}
