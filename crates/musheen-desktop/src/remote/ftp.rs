use super::{
    ConnectionProfile, CredentialResolver, OpendalStore, RemoteCasePolicy, RemoteError,
    RemoteErrorCategory, RemoteMutationPolicy, RemoteProtocol, SecurityPolicy, TlsPolicy,
};
use musheen_core::{BoxFuture, CancellationToken, ProviderId};
use opendal::{Operator, services::Ftp};

use super::opendal_store::{
    RemoteErrorContext, classify_opendal_error, profile_endpoint, profile_error, profile_password,
};

/// Wraps an already authenticated FTP operator with Musheen's provider contract.
/// Connection-profile construction remains unavailable until credentials can be
/// supplied without copying them outside Secret Service.
pub fn ftp_store(
    provider: ProviderId,
    operator: Operator,
    case_policy: RemoteCasePolicy,
) -> Result<OpendalStore, RemoteError> {
    OpendalStore::from_operator(
        provider,
        RemoteProtocol::Ftp,
        operator,
        case_policy,
        RemoteMutationPolicy::CapabilitiesVerified,
    )
}

/// FTPS uses OpenDAL's FTP service but keeps a distinct protocol identity.
pub fn ftps_store(
    provider: ProviderId,
    operator: Operator,
    case_policy: RemoteCasePolicy,
) -> Result<OpendalStore, RemoteError> {
    OpendalStore::from_operator(
        provider,
        RemoteProtocol::Ftps,
        operator,
        case_policy,
        RemoteMutationPolicy::CapabilitiesVerified,
    )
}

pub fn ftp_store_from_profile<'a, R: CredentialResolver>(
    provider: ProviderId,
    profile: &'a ConnectionProfile,
    credentials: &'a R,
    cancellation: CancellationToken,
) -> BoxFuture<'a, Result<OpendalStore, RemoteError>> {
    Box::pin(async move {
        let _ = futures_rustls::rustls::crypto::ring::default_provider().install_default();
        opendal::install_default();
        if !matches!(
            profile.protocol(),
            RemoteProtocol::Ftp | RemoteProtocol::Ftps
        ) || profile.proxy().is_some()
        {
            return Err(profile_error(profile, RemoteErrorCategory::Unsupported));
        }
        let (scheme, port) = match (profile.protocol(), profile.security()) {
            (RemoteProtocol::Ftp, SecurityPolicy::PlaintextConfirmed) => ("ftp", 21),
            (RemoteProtocol::Ftps, SecurityPolicy::Tls(TlsPolicy::SystemRoots)) => ("ftps", 990),
            (RemoteProtocol::Ftps, SecurityPolicy::Tls(TlsPolicy::PinnedSha256(_))) => {
                // OpenDAL's FTP service hard-wires native roots and exposes no
                // certificate verifier hook. Never silently weaken the pin.
                return Err(profile_error(profile, RemoteErrorCategory::Unsupported));
            }
            _ => return Err(profile_error(profile, RemoteErrorCategory::InvalidProfile)),
        };
        let password = profile_password(profile, credentials, cancellation).await?;
        let mut builder = Ftp::default()
            .endpoint(&profile_endpoint(profile, scheme, port))
            .root(profile.path())
            .user(profile.username().unwrap_or("anonymous"));
        if let Some(password) = password.as_deref() {
            builder = builder.password(password);
        }
        let operator = Operator::new(builder).map_err(|error| {
            profile_error(
                profile,
                classify_opendal_error(&error, RemoteErrorContext::Connect),
            )
        })?;
        OpendalStore::from_profile_operator(
            provider,
            profile,
            operator,
            RemoteCasePolicy::Unknown,
            RemoteMutationPolicy::CapabilitiesVerified,
        )
    })
}
