use super::transfer::{local_staging_budget, publish_remote_verified};
use super::*;
use musheen_core::CommandTargetRef;
use musheen_local::{
    DropAction, LocalOperationFailure, ProviderTransferExecution, TransferOutcome,
};
use musheen_ops::{
    CopyStrategy, MoveMetadataReview, ProviderLimits, ProviderSnapshot, RemoteTransferCapabilities,
    RemoteTransferPlan, RemoteTransferStrategy, StagingPath,
};
use std::fmt;

const VERIFY_CHUNK_BYTES: u64 = 1024 * 1024;

pub(crate) struct RemoteRelayRoute {
    source: Arc<RemoteProfileStore>,
    destination: Arc<RemoteProfileStore>,
}

impl RemoteRelayRoute {
    pub(crate) fn new(
        source: Arc<RemoteProfileStore>,
        destination: Arc<RemoteProfileStore>,
    ) -> Self {
        Self {
            source,
            destination,
        }
    }

    async fn execute_copy(
        &self,
        execution: ProviderTransferExecution<'_>,
    ) -> Result<(TransferOutcome, StoreItem), LocalOperationFailure> {
        let cancellation = execution.cancellation().clone();
        cancellation
            .check()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let source_store = self
            .source
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let destination_store = self
            .destination
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        if !destination_store.supports_exclusive_publish() {
            return Err(LocalOperationFailure::failed(
                "the remote destination cannot publish without replacing an existing file",
            ));
        }
        let source = source_store
            .resolve_item(execution.source())
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
            .ok_or_else(|| LocalOperationFailure::failed("the remote source disappeared"))?;
        if source.kind() != ItemKind::RegularFile {
            return Err(LocalOperationFailure::failed(
                "remote relay currently requires a regular file",
            ));
        }
        if execution.action() == DropAction::Move && !source_store.supports_conditional_delete() {
            return Err(LocalOperationFailure::failed(
                "the remote service cannot conditional delete a reviewed source",
            ));
        }
        if execution
            .expected_identity()
            .is_some_and(|expected| expected != source.id())
        {
            return Err(LocalOperationFailure::failed(
                "the remote source changed before the copy",
            ));
        }
        let size = source
            .size()
            .ok_or_else(|| LocalOperationFailure::failed("the remote source size is unknown"))?;
        if destination_store
            .resolve_item(execution.destination())
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
            .is_some()
        {
            return Err(LocalOperationFailure::failed(
                "the remote destination already exists",
            ));
        }
        let source_snapshot = ProviderSnapshot::new(
            self.source.provider_id().clone(),
            source_store.capabilities(execution.source()),
            ProviderLimits::default(),
        );
        let destination_snapshot = self.provider_snapshot(execution.destination());
        let source_server_copy = source_store.supports_server_copy();
        let destination_server_copy = destination_store.supports_server_copy();
        let source_io = if source_server_copy {
            RemoteTransferCapabilities::readable().with_server_copy()
        } else {
            RemoteTransferCapabilities::readable()
        };
        let destination_io = if destination_server_copy {
            RemoteTransferCapabilities::writable().with_server_copy()
        } else {
            RemoteTransferCapabilities::writable()
        };
        let plan = RemoteTransferPlan::new(
            musheen_ops::OperationKind::Copy,
            &source_snapshot,
            &destination_snapshot,
            source_io,
            destination_io,
        )
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let staging = StagingPath::for_slash_key_destination_with_nonce(
            execution.destination(),
            execution.id(),
            execution.generation(),
            StagingPath::unique_nonce(),
        )
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        if plan.strategy() == RemoteTransferStrategy::ServerSideCopy {
            stage_server_copy(
                &source_store,
                execution.source(),
                &staging,
                cancellation.clone(),
            )
            .await?;
        } else {
            stage_streamed_copy(
                &source_store,
                execution.source(),
                &destination_store,
                &staging,
                size,
                cancellation.clone(),
            )
            .await?;
        }
        let staged = destination_store
            .resolve_item(staging.path())
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?
            .ok_or_else(|| {
                LocalOperationFailure::recoverable(
                    "the remote staging file is missing",
                    staging.path().clone(),
                )
            })?;
        if staged.size() != Some(size) {
            return Err(LocalOperationFailure::recoverable(
                "the remote staging size does not match the source",
                staging.path().clone(),
            ));
        }
        let matches = remote_contents_match(
            &source_store,
            execution.source(),
            &destination_store,
            staging.path(),
            size,
            cancellation.clone(),
        )
        .await
        .map_err(|error| LocalOperationFailure::recoverable(error, staging.path().clone()))?;
        if !matches {
            return Err(LocalOperationFailure::recoverable(
                "remote staging content failed verification",
                staging.path().clone(),
            ));
        }
        let current = source_store
            .resolve_item(execution.source())
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?;
        if current.as_ref().is_none_or(|item| item.id() != source.id()) {
            return Err(LocalOperationFailure::recoverable(
                "the remote source changed during the copy",
                staging.path().clone(),
            ));
        }
        if destination_store
            .resolve_item(execution.destination())
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?
            .is_some()
        {
            return Err(LocalOperationFailure::recoverable(
                "the remote destination appeared during the copy",
                staging.path().clone(),
            ));
        }
        let outcome = publish_remote_verified(
            &destination_store,
            &staging,
            execution.destination(),
            size,
            cancellation,
        )
        .await?;
        Ok((outcome, source))
    }

    async fn finalize_reviewed_move(
        &self,
        review: &MoveMetadataReview,
        cancellation: CancellationToken,
    ) -> Result<CommandTargetRef, LocalOperationFailure> {
        let (source_identity, source_size) = review.remote_source_proof().ok_or_else(|| {
            LocalOperationFailure::needs_attention("the reviewed relay has no remote source proof")
        })?;
        let destination_identity = review.destination_identity().ok_or_else(|| {
            LocalOperationFailure::needs_attention("the reviewed relay has no destination identity")
        })?;
        let source_store = self
            .source
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        let destination_store = self
            .destination
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        let source = source_store
            .resolve_item(review.source())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?
            .ok_or_else(|| {
                LocalOperationFailure::needs_attention("the reviewed source disappeared")
            })?;
        let destination = destination_store
            .resolve_item(review.destination())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?
            .ok_or_else(|| {
                LocalOperationFailure::needs_attention("the reviewed destination disappeared")
            })?;
        if source.id() != source_identity || source.size() != Some(source_size) {
            return Err(LocalOperationFailure::needs_attention(
                "the reviewed remote source changed",
            ));
        }
        if destination.id() != destination_identity || destination.size() != Some(source_size) {
            return Err(LocalOperationFailure::needs_attention(
                "the reviewed remote destination changed",
            ));
        }
        let matches = remote_contents_match(
            &source_store,
            review.source(),
            &destination_store,
            review.destination(),
            source_size,
            cancellation.clone(),
        )
        .await
        .map_err(LocalOperationFailure::needs_attention)?;
        if !matches {
            return Err(LocalOperationFailure::needs_attention(
                "the published remote destination no longer matches the source",
            ));
        }
        if destination_store
            .resolve_item(review.destination())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?
            .as_ref()
            .is_none_or(|item| item.id() != destination_identity)
        {
            return Err(LocalOperationFailure::needs_attention(
                "the remote destination changed during final verification",
            ));
        }
        source_store.delete_if_unchanged(review.source(), source_identity, cancellation).await
            .map_err(|error| LocalOperationFailure::needs_attention(format!(
                "the destination was published but remote source removal needs inspection: {error}"
            )))?;
        CommandTargetRef::new(destination.id().clone(), destination.path().clone())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))
    }
}

impl fmt::Debug for RemoteRelayRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteRelayRoute")
            .field("source_provider", self.source.provider_id())
            .field("destination_provider", self.destination.provider_id())
            .finish()
    }
}

impl ProviderTransferRoute for RemoteRelayRoute {
    fn source_provider_id(&self) -> &ProviderId {
        self.source.provider_id()
    }

    fn destination_provider_id(&self) -> &ProviderId {
        self.destination.provider_id()
    }

    fn plan_destination(
        &self,
        _action: DropAction,
        source: &StorePath,
        target: &StorePath,
        _expected_identity: Option<&ItemId>,
    ) -> Result<StorePath, Box<str>> {
        let (source_provider, source_key) = source
            .provider_key()
            .ok_or("the source is not a remote path")?;
        let (destination_provider, target_key) = target
            .provider_key()
            .ok_or("the destination is not a remote path")?;
        if source_provider != self.source.provider_id()
            || destination_provider != self.destination.provider_id()
            || !source_key.starts_with(b"/")
            || !target_key.starts_with(b"/")
        {
            return Err("the remote path belongs to another provider".into());
        }
        let name = source_key
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or_default();
        if name.is_empty() || name == b"." || name == b".." || std::str::from_utf8(name).is_err() {
            return Err("the remote source has no safe UTF-8 file name".into());
        }
        let mut destination = target_key.to_vec();
        if !destination.ends_with(b"/") {
            destination.push(b'/');
        }
        destination.extend_from_slice(name);
        let destination = StorePath::from_provider_key(destination_provider.clone(), destination)
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        if &destination == source {
            return Err("the remote source is already in that directory".into());
        }
        Ok(destination)
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let (outcome, source) = runtime.block_on(self.execute_copy(execution))?;
        if execution.action() == DropAction::Copy {
            return Ok(outcome);
        }
        let TransferOutcome::Completed(target) = outcome else {
            return Err(LocalOperationFailure::needs_attention(
                "the remote move did not produce a completed destination",
            ));
        };
        let source_size = source.size().ok_or_else(|| {
            LocalOperationFailure::needs_attention("the reviewed remote source has no size")
        })?;
        let review = MoveMetadataReview::new_remote(
            execution.source().clone(),
            execution.destination().clone(),
            source.id().clone(),
            source_size,
            CopyStrategy::Streamed,
            super::transfer::remote_move_metadata_loss(),
        )
        .with_destination_identity(target.id().clone());
        Ok(TransferOutcome::MetadataReview {
            target,
            review: Box::new(review),
        })
    }

    fn finalize_move(
        &self,
        review: &MoveMetadataReview,
        cancellation: &CancellationToken,
    ) -> Result<CommandTargetRef, LocalOperationFailure> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        runtime.block_on(self.finalize_reviewed_move(review, cancellation.clone()))
    }
}

async fn stage_server_copy(
    source_store: &OpendalStore,
    source: &StorePath,
    staging: &StagingPath,
    cancellation: CancellationToken,
) -> Result<(), LocalOperationFailure> {
    source_store
        .mutate(
            MutationRequest::Copy {
                source: source.clone(),
                destination: staging.path().clone(),
            },
            cancellation,
        )
        .await
        .map_err(|error| {
            LocalOperationFailure::recoverable(
                format!("server-side copy stopped: {error}"),
                staging.path().clone(),
            )
        })
}

async fn stage_streamed_copy(
    source_store: &OpendalStore,
    source: &StorePath,
    destination_store: &OpendalStore,
    staging: &StagingPath,
    size: u64,
    cancellation: CancellationToken,
) -> Result<(), LocalOperationFailure> {
    let temporary = tempfile::Builder::new()
        .prefix("musheen-remote-transfer-")
        .tempdir()
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
    let budget = local_staging_budget(temporary.path())?;
    if size > budget {
        return Err(LocalOperationFailure::failed(
            "the remote file exceeds the available relay staging budget",
        ));
    }
    let local_file = temporary.path().join("payload");
    source_store
        .download_to_new_local(source, &local_file, budget, cancellation.clone())
        .await
        .map_err(|error| {
            LocalOperationFailure::failed(format!("remote relay download stopped: {error}"))
        })?;
    destination_store
        .upload_staging_from_local(&local_file, staging, cancellation)
        .await
        .map_err(|error| {
            LocalOperationFailure::recoverable(
                format!("remote relay upload stopped: {error}"),
                staging.path().clone(),
            )
        })?;
    Ok(())
}

async fn remote_contents_match(
    source: &OpendalStore,
    source_path: &StorePath,
    destination: &OpendalStore,
    destination_path: &StorePath,
    size: u64,
    cancellation: CancellationToken,
) -> Result<bool, Box<str>> {
    let mut offset = 0_u64;
    while offset < size {
        cancellation
            .check()
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        let end = offset.saturating_add(VERIFY_CHUNK_BYTES).min(size);
        let original = source
            .read_range(source_path, offset..end, cancellation.clone())
            .await
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        let copied = destination
            .read_range(destination_path, offset..end, cancellation.clone())
            .await
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        if original.len() as u64 != end - offset || original != copied {
            return Ok(false);
        }
        offset = end;
    }
    Ok(true)
}
