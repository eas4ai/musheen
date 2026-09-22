use musheen_core::{BoxFuture, CancellationToken};
use std::collections::HashMap;
use std::future::poll_fn;
use std::task::Poll;
use std::time::Duration;
use zbus::zvariant::OwnedValue;

use super::{
    AuthorizationError, AuthorizationGrant, AuthorizationRequest, Authorizer, Clock,
    PrivilegeProvider, SystemClock,
};

const SERVICE: &str = "org.freedesktop.PolicyKit1";
const PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const ALLOW_USER_INTERACTION: u32 = 1;

pub trait PolkitConnectionFactory: Clone + Send + Sync + 'static {
    fn connect(&self) -> BoxFuture<'_, Result<zbus::Connection, AuthorizationError>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemBusPolkitConnection;

impl PolkitConnectionFactory for SystemBusPolkitConnection {
    fn connect(&self) -> BoxFuture<'_, Result<zbus::Connection, AuthorizationError>> {
        Box::pin(async {
            zbus::Connection::system()
                .await
                .map_err(|_| AuthorizationError::Unavailable)
        })
    }
}

#[derive(Clone)]
pub struct PolkitAuthorizer<C = SystemBusPolkitConnection, K = SystemClock> {
    connection: C,
    clock: K,
    timeout: Duration,
    grant_lifetime: Duration,
    allow_user_interaction: bool,
}

impl Default for PolkitAuthorizer<SystemBusPolkitConnection, SystemClock> {
    fn default() -> Self {
        Self {
            connection: SystemBusPolkitConnection,
            clock: SystemClock,
            timeout: Duration::from_secs(60),
            grant_lifetime: Duration::from_secs(60),
            allow_user_interaction: true,
        }
    }
}

impl<C: PolkitConnectionFactory, K: Clock> PolkitAuthorizer<C, K> {
    #[must_use]
    pub fn with_connection(connection: C, clock: K) -> Self {
        Self {
            connection,
            clock,
            timeout: Duration::from_secs(60),
            grant_lifetime: Duration::from_secs(60),
            allow_user_interaction: true,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_user_interaction(mut self, allow: bool) -> Self {
        self.allow_user_interaction = allow;
        self
    }

    async fn authorize_async(
        &self,
        request: &AuthorizationRequest,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        if request.provider() != PrivilegeProvider::Polkit {
            return Err(AuthorizationError::Unavailable);
        }
        let connection = self.connection.connect().await?;
        let proxy = zbus::Proxy::new(&connection, SERVICE, PATH, INTERFACE)
            .await
            .map_err(|_| AuthorizationError::Unavailable)?;
        let mut subject_details = HashMap::<String, OwnedValue>::new();
        subject_details.insert("pid".to_owned(), OwnedValue::from(request.subject().pid()));
        subject_details.insert("uid".to_owned(), OwnedValue::from(request.subject().uid()));
        subject_details.insert(
            "start-time".to_owned(),
            OwnedValue::from(request.subject().start_time()),
        );
        let subject = ("unix-process", subject_details);
        let details = HashMap::from([
            ("command".to_owned(), request.action_id().to_owned()),
            (
                "target".to_owned(),
                request
                    .target()
                    .to_str()
                    .unwrap_or("non-utf8-local-path")
                    .to_owned(),
            ),
            (
                "request-digest".to_owned(),
                request.binding_digest().to_hex().to_string(),
            ),
        ]);
        let parameters = (
            subject,
            request.action_id(),
            details,
            if self.allow_user_interaction {
                ALLOW_USER_INTERACTION
            } else {
                0
            },
            request.request_id(),
        );
        let call = proxy
            .call::<_, _, (bool, bool, HashMap<String, String>)>("CheckAuthorization", &parameters);
        enum Completion<T> {
            Reply(T),
            Cancelled,
            TimedOut,
        }
        let cancellation_wait = async {
            poll_fn(|context| {
                if cancellation.is_cancelled() {
                    Poll::Ready(())
                } else {
                    cancellation.register_waker(context.waker());
                    Poll::Pending
                }
            })
            .await;
            Completion::Cancelled
        };
        let deadline = async {
            async_io::Timer::after(timeout.min(self.timeout)).await;
            Completion::TimedOut
        };
        let completion = futures_lite::future::race(
            async { Completion::Reply(call.await) },
            futures_lite::future::race(cancellation_wait, deadline),
        )
        .await;
        let (authorized, challenge, _) = match completion {
            Completion::Reply(result) => result.map_err(|_| AuthorizationError::Unavailable)?,
            Completion::Cancelled => {
                let cancellation_id = (request.request_id(),);
                let cancel = proxy.call::<_, _, ()>("CancelCheckAuthorization", &cancellation_id);
                let _ = futures_lite::future::race(cancel, async {
                    async_io::Timer::after(Duration::from_millis(100)).await;
                    Err(zbus::Error::Failure(
                        "polkit cancellation timed out".to_owned(),
                    ))
                })
                .await;
                return Err(AuthorizationError::Cancelled);
            }
            Completion::TimedOut => return Err(AuthorizationError::Unavailable),
        };
        if !authorized {
            return Err(if challenge {
                AuthorizationError::Cancelled
            } else {
                AuthorizationError::Denied
            });
        }
        let lifetime = self
            .grant_lifetime
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        Ok(AuthorizationGrant::new(
            format!("uid:{}", request.subject().uid()),
            self.clock.now_unix_millis().saturating_add(lifetime),
        ))
    }
}

impl<C: PolkitConnectionFactory, K: Clock> Authorizer for PolkitAuthorizer<C, K> {
    fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        futures_lite::future::block_on(self.authorize_async(
            request,
            &CancellationToken::new(),
            self.timeout,
        ))
    }

    fn authorize_cancellable(
        &self,
        request: &AuthorizationRequest,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        futures_lite::future::block_on(self.authorize_async(request, cancellation, timeout))
    }
}
