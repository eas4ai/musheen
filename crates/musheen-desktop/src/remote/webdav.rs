use super::{
    ConnectionProfile, CredentialResolver, OpendalStore, RemoteCasePolicy, RemoteError,
    RemoteErrorCategory, RemoteMutationPolicy, RemoteProtocol, SecurityPolicy, TlsPolicy,
};
use musheen_core::{BoxFuture, CancellationToken, ProviderId};
use opendal::{Operator, services::Webdav};

use super::opendal_store::{
    RemoteErrorContext, classify_opendal_error, profile_endpoint, profile_error, profile_password,
    with_pinned_http_transport,
};

/// Wraps a configured WebDAV operator. Mutations are still checked against the
/// operator's runtime capability report before dispatch.
pub fn webdav_store(
    provider: ProviderId,
    operator: Operator,
    case_policy: RemoteCasePolicy,
) -> Result<OpendalStore, RemoteError> {
    OpendalStore::from_operator(
        provider,
        RemoteProtocol::WebDav,
        operator,
        case_policy,
        RemoteMutationPolicy::CapabilitiesVerified,
    )
}

pub fn webdav_store_from_profile<'a, R: CredentialResolver>(
    provider: ProviderId,
    profile: &'a ConnectionProfile,
    credentials: &'a R,
    cancellation: CancellationToken,
) -> BoxFuture<'a, Result<OpendalStore, RemoteError>> {
    Box::pin(async move {
        opendal::install_default();
        if profile.protocol() != RemoteProtocol::WebDav || profile.proxy().is_some() {
            return Err(profile_error(profile, RemoteErrorCategory::Unsupported));
        }
        let (scheme, port, pin) = match profile.security() {
            SecurityPolicy::PlaintextConfirmed => ("http", 80, None),
            SecurityPolicy::Tls(TlsPolicy::SystemRoots) => ("https", 443, None),
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)) => ("https", 443, Some(*pin)),
            _ => return Err(profile_error(profile, RemoteErrorCategory::InvalidProfile)),
        };
        let password = profile_password(profile, credentials, cancellation).await?;
        let mut builder = Webdav::default()
            .endpoint(&profile_endpoint(profile, scheme, port))
            .root(profile.path());
        if let Some(secret) = password.as_deref() {
            builder = match profile.username() {
                Some(username) => builder.username(username).password(secret),
                None => builder.token(secret),
            };
        }
        let mut operator = Operator::new(builder).map_err(|error| {
            profile_error(
                profile,
                classify_opendal_error(&error, RemoteErrorContext::Connect),
            )
        })?;
        if let Some(pin) = pin {
            operator = with_pinned_http_transport(operator, profile, pin)?;
        }
        OpendalStore::from_profile_operator(
            provider,
            profile,
            operator,
            RemoteCasePolicy::Unknown,
            RemoteMutationPolicy::CapabilitiesVerified,
        )
    })
}
