use super::*;
use musheen_core::CommandTargetRef;
use musheen_local::{
    DropAction, LocalOperationFailure, ProviderTransferExecution, TransferOutcome,
};
use musheen_ops::{
    ProviderLimits, ProviderSnapshot, RemoteTransferCapabilities, RemoteTransferPlan, StagingPath,
};
use std::fmt;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const VERIFY_CHUNK_BYTES: usize = 1024 * 1024;

pub(crate) struct RemoteUploadRoute {
    source_provider: ProviderId,
    destination: Arc<RemoteProfileStore>,
}

impl RemoteUploadRoute {
    pub(crate) fn new(destination: Arc<RemoteProfileStore>) -> Self {
        Self {
            source_provider: ProviderId::new("local").expect("the local provider ID is valid"),
            destination,
        }
    }

    async fn execute_copy(
        &self,
        execution: ProviderTransferExecution<'_>,
    ) -> Result<TransferOutcome, LocalOperationFailure> {
        let source_path = execution.source().as_unix_path().ok_or_else(|| {
            LocalOperationFailure::failed("the transfer source is not a local path")
        })?;
        let local = LocalStore::new();
        let source = local
            .resolve_item(execution.source())
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
            .ok_or_else(|| LocalOperationFailure::failed("the transfer source disappeared"))?;
        if source.kind() != ItemKind::RegularFile {
            return Err(LocalOperationFailure::failed(
                "remote upload currently requires a regular file",
            ));
        }
        if execution
            .expected_identity()
            .is_some_and(|expected| expected != source.id())
        {
            return Err(LocalOperationFailure::failed(
                "the source changed before the remote transfer",
            ));
        }
        let source_size = source
            .size()
            .ok_or_else(|| LocalOperationFailure::failed("the source size is unknown"))?;
        let cancellation = execution.cancellation().clone();
        cancellation
            .check()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let remote = self
            .destination
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        if !remote.supports_exclusive_publish() {
            return Err(LocalOperationFailure::failed(
                "the remote service cannot publish a new file without replacing an existing one",
            ));
        }
        let source_snapshot = ProviderSnapshot::new(
            self.source_provider.clone(),
            local.capabilities(execution.source()),
            ProviderLimits::default(),
        );
        let destination_snapshot = self.provider_snapshot(execution.destination());
        let _plan = RemoteTransferPlan::new(
            musheen_ops::OperationKind::Copy,
            &source_snapshot,
            &destination_snapshot,
            RemoteTransferCapabilities::readable(),
            RemoteTransferCapabilities::writable(),
        )
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        if remote
            .resolve_item(execution.destination())
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
            .is_some()
        {
            return Err(LocalOperationFailure::failed(
                "the remote destination already exists",
            ));
        }
        let staging = StagingPath::for_slash_key_destination_with_nonce(
            execution.destination(),
            execution.id(),
            execution.generation(),
            StagingPath::unique_nonce(),
        )
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let uploaded = remote
            .upload_staging_from_local(source_path, &staging, cancellation.clone())
            .await
            .map_err(|error| {
                LocalOperationFailure::recoverable(
                    format!("remote upload stopped: {error}"),
                    staging.path().clone(),
                )
            })?;
        if uploaded != source_size {
            return Err(LocalOperationFailure::recoverable(
                "the remote staging size does not match the source",
                staging.path().clone(),
            ));
        }
        verify_upload(
            &remote,
            source_path,
            &staging,
            source_size,
            cancellation.clone(),
        )
        .await?;
        let current = local.resolve_item(execution.source()).map_err(|error| {
            LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
        })?;
        if current.as_ref().is_none_or(|item| item.id() != source.id()) {
            return Err(LocalOperationFailure::recoverable(
                "the source changed during the remote upload",
                staging.path().clone(),
            ));
        }
        if remote
            .resolve_item(execution.destination())
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?
            .is_some()
        {
            return Err(LocalOperationFailure::recoverable(
                "the remote destination appeared during the upload",
                staging.path().clone(),
            ));
        }
        remote
            .publish_staging_noreplace(&staging, execution.destination(), cancellation.clone())
            .await
            .map_err(|error| {
                LocalOperationFailure::needs_attention_with_staging(
                    format!("remote publication may have completed: {error}"),
                    staging.path().clone(),
                )
            })?;
        let completed = remote
            .resolve_item(execution.destination())
            .map_err(|error| {
                LocalOperationFailure::needs_attention_with_staging(
                    format!("published remote destination could not be inspected: {error}"),
                    staging.path().clone(),
                )
            })?
            .ok_or_else(|| {
                LocalOperationFailure::needs_attention_with_staging(
                    "published remote destination is missing",
                    staging.path().clone(),
                )
            })?;
        if completed.size() != Some(source_size) {
            return Err(LocalOperationFailure::needs_attention_with_staging(
                "published remote destination has the wrong size",
                staging.path().clone(),
            ));
        }
        remote
            .mutate(
                MutationRequest::PermanentDelete {
                    target: staging.path().clone(),
                },
                CancellationToken::new(),
            )
            .await
            .map_err(|error| {
                LocalOperationFailure::needs_attention_with_staging(
                    format!("remote copy finished but staging cleanup failed: {error}"),
                    staging.path().clone(),
                )
            })?;
        let target = CommandTargetRef::new(completed.id().clone(), completed.path().clone())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        Ok(TransferOutcome::Completed(target))
    }
}

impl fmt::Debug for RemoteUploadRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteUploadRoute")
            .field("destination_provider", self.destination.provider_id())
            .finish()
    }
}

impl ProviderTransferRoute for RemoteUploadRoute {
    fn source_provider_id(&self) -> &ProviderId {
        &self.source_provider
    }

    fn destination_provider_id(&self) -> &ProviderId {
        self.destination.provider_id()
    }

    fn plan_destination(
        &self,
        action: DropAction,
        source: &StorePath,
        target: &StorePath,
        _expected_identity: Option<&ItemId>,
    ) -> Result<StorePath, Box<str>> {
        if action != DropAction::Copy {
            return Err("remote moves require metadata review and are not ready".into());
        }
        let source = source
            .as_unix_path()
            .ok_or("the upload source is not a local path")?;
        let name = source
            .file_name()
            .ok_or("the upload source has no file name")?
            .as_bytes();
        if name.is_empty() || name == b"." || name == b".." || std::str::from_utf8(name).is_err() {
            return Err("the remote destination requires a UTF-8 file name".into());
        }
        let (provider, key) = target
            .provider_key()
            .ok_or("the upload target is not a remote location")?;
        if provider != self.destination.provider_id() || !key.starts_with(b"/") {
            return Err("the upload target belongs to another provider".into());
        }
        let mut destination = key.to_vec();
        if !destination.ends_with(b"/") {
            destination.push(b'/');
        }
        destination.extend_from_slice(name);
        StorePath::from_provider_key(provider.clone(), destination)
            .map_err(|error| error.to_string().into())
    }

    fn provider_snapshot(&self, location: &StorePath) -> ProviderSnapshot {
        ProviderSnapshot::new(
            self.destination.provider_id().clone(),
            self.destination.capabilities(location),
            ProviderLimits::default(),
        )
    }

    fn execute_transfer(
        &self,
        execution: ProviderTransferExecution<'_>,
    ) -> Result<TransferOutcome, LocalOperationFailure> {
        if execution.action() != DropAction::Copy {
            return Err(LocalOperationFailure::failed(
                "remote moves require metadata review and are not ready",
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        runtime.block_on(self.execute_copy(execution))
    }
}

async fn verify_upload(
    remote: &OpendalStore,
    source: &Path,
    staging: &StagingPath,
    size: u64,
    cancellation: CancellationToken,
) -> Result<(), LocalOperationFailure> {
    let mut source = std::fs::File::open(source).map_err(|error| {
        LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
    })?;
    let mut local = vec![0; VERIFY_CHUNK_BYTES];
    let mut offset = 0_u64;
    while offset < size {
        cancellation.check().map_err(|error| {
            LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
        })?;
        let end = offset.saturating_add(VERIFY_CHUNK_BYTES as u64).min(size);
        let count = (end - offset) as usize;
        source.read_exact(&mut local[..count]).map_err(|error| {
            LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
        })?;
        let copied = remote
            .read_range(staging.path(), offset..end, cancellation.clone())
            .await
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?;
        if copied != local[..count] {
            return Err(LocalOperationFailure::recoverable(
                "remote staging content failed verification",
                staging.path().clone(),
            ));
        }
        offset = end;
    }
    Ok(())
}
