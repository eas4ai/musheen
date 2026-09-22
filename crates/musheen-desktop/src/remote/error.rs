use super::{RemoteHost, RemoteProtocol};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteErrorCategory {
    InvalidProfile,
    Authentication,
    HostKey,
    Tls,
    Network,
    Protocol,
    Timeout,
    Cancelled,
    Saturated,
    Unavailable,
}

/// A credential-safe provider error. Transport text is deliberately not kept
/// because third-party errors often echo URLs, usernames, or proxy credentials.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteError {
    protocol: RemoteProtocol,
    category: RemoteErrorCategory,
    host: Option<RemoteHost>,
}

impl RemoteError {
    #[must_use]
    pub const fn new(
        protocol: RemoteProtocol,
        category: RemoteErrorCategory,
        host: Option<RemoteHost>,
    ) -> Self {
        Self {
            protocol,
            category,
            host,
        }
    }

    #[must_use]
    pub const fn protocol(&self) -> RemoteProtocol {
        self.protocol
    }

    #[must_use]
    pub const fn category(&self) -> RemoteErrorCategory {
        self.category
    }

    #[must_use]
    pub fn host(&self) -> Option<&RemoteHost> {
        self.host.as_ref()
    }
}

impl fmt::Debug for RemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteError")
            .field("protocol", &self.protocol)
            .field("category", &self.category)
            .field("host", &self.host)
            .finish()
    }
}

impl fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?} {:?}", self.protocol, self.category)?;
        if let Some(host) = &self.host {
            write!(formatter, " for {host}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RemoteError {}
