use super::{
    CONNECT_TIMEOUT, ConnectionProfile, RemoteError, RemoteErrorCategory, RemoteHost,
    RemoteProtocol, SecurityPolicy,
};
use musheen_core::{BoxFuture, CancellationToken};
use std::future::poll_fn;
use std::task::Poll;

pub trait ConnectionProbe: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        host: &'a RemoteHost,
        port: u16,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnectionProbe;

impl ConnectionProbe for TcpConnectionProbe {
    fn connect<'a>(
        &'a self,
        host: &'a RemoteHost,
        port: u16,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>> {
        Box::pin(async move {
            futures_lite::future::race(
                async {
                    async_net::TcpStream::connect((host.as_str(), port))
                        .await
                        .map(|_| ())
                        .map_err(|_| RemoteErrorCategory::Network)
                },
                async {
                    cancelled(cancellation).await;
                    Err(RemoteErrorCategory::Cancelled)
                },
            )
            .await
        })
    }
}

pub trait ProfileConnectionTest: Send + Sync + 'static {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>>;
}

#[derive(Clone, Debug)]
pub struct ProfileConnectionTester<P = TcpConnectionProbe> {
    probe: P,
}

impl<P> ProfileConnectionTester<P> {
    #[must_use]
    pub const fn new(probe: P) -> Self {
        Self { probe }
    }
}

impl Default for ProfileConnectionTester<TcpConnectionProbe> {
    fn default() -> Self {
        Self::new(TcpConnectionProbe)
    }
}

impl<P: ConnectionProbe> ProfileConnectionTest for ProfileConnectionTester<P> {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            let timeout_cancellation = cancellation.clone();
            let result = futures_lite::future::race(
                self.probe.connect(
                    profile.host(),
                    profile.port().unwrap_or_else(|| default_port(profile)),
                    cancellation,
                ),
                async move {
                    futures_lite::future::race(
                        async {
                            cancelled(timeout_cancellation).await;
                            Err(RemoteErrorCategory::Cancelled)
                        },
                        async {
                            async_io::Timer::after(CONNECT_TIMEOUT).await;
                            Err(RemoteErrorCategory::Timeout)
                        },
                    )
                    .await
                },
            )
            .await;
            result.map_err(|category| {
                RemoteError::new(profile.protocol(), category, Some(profile.host().clone()))
            })
        })
    }
}

fn default_port(profile: &ConnectionProfile) -> u16 {
    match profile.protocol() {
        RemoteProtocol::Ftp => 21,
        RemoteProtocol::Ftps => 990,
        RemoteProtocol::Sftp => 22,
        RemoteProtocol::WebDav | RemoteProtocol::Http => {
            if matches!(profile.security(), SecurityPolicy::PlaintextConfirmed) {
                80
            } else {
                443
            }
        }
        RemoteProtocol::Smb => 445,
        RemoteProtocol::Nfs => 2049,
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
