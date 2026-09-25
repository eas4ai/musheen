use super::*;
use musheen_core::CommandTargetRef;
use musheen_local::{
    DropAction, LocalOperationFailure, ProviderTransferExecution, TransferOutcome,
};
use musheen_ops::{
    CopyProvider, CopyStrategy, MetadataKind, MetadataReport, MoveMetadataReview, ProviderError,
    ProviderLimits, ProviderSnapshot, RemoteTransferCapabilities, RemoteTransferPlan, StagingPath,
    source_unchanged,
};
use nix::sys::statvfs::statvfs;
use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const MAX_LOCAL_STAGING_BYTES: u64 = 10 * 1024 * 1024 * 1024;

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
            Store::capabilities(&local, execution.source()),
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
        let matches = remote
            .local_content_matches(
                source_path,
                staging.path(),
                source_size,
                cancellation.clone(),
            )
            .await
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?;
        if !matches {
            return Err(LocalOperationFailure::recoverable(
                "remote staging content failed verification",
                staging.path().clone(),
            ));
        }
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
        publish_remote_verified(
            &remote,
            &staging,
            execution.destination(),
            source_size,
            cancellation,
        )
        .await
    }

    async fn finalize_reviewed_move(
        &self,
        review: &MoveMetadataReview,
        cancellation: CancellationToken,
    ) -> Result<CommandTargetRef, LocalOperationFailure> {
        let expected_destination = review.destination_identity().ok_or_else(|| {
            LocalOperationFailure::needs_attention(
                "the reviewed remote destination has no recorded identity",
            )
        })?;
        let remote = self
            .destination
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        let completed = remote
            .resolve_item(review.destination())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?
            .ok_or_else(|| {
                LocalOperationFailure::needs_attention("the reviewed destination disappeared")
            })?;
        if completed.id() != expected_destination
            || completed.size() != Some(review.source_snapshot().size())
        {
            return Err(LocalOperationFailure::needs_attention(
                "the reviewed destination changed before source removal",
            ));
        }
        let mut local = LocalStore::new();
        let current = CopyProvider::inspect(&mut local, review.source(), false)
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        if !source_unchanged(review.source_snapshot(), &current) {
            return Err(LocalOperationFailure::needs_attention(
                "the local source changed after the remote copy",
            ));
        }
        let source_path = review.source().as_unix_path().ok_or_else(|| {
            LocalOperationFailure::needs_attention("the reviewed source is not local")
        })?;
        let matches = remote
            .local_content_matches(
                source_path,
                review.destination(),
                review.source_snapshot().size(),
                cancellation.clone(),
            )
            .await
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        if !matches {
            return Err(LocalOperationFailure::needs_attention(
                "the published remote content no longer matches the source",
            ));
        }
        cancellation
            .check()
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        if remote
            .resolve_item(review.destination())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?
            .as_ref()
            .is_none_or(|item| item.id() != expected_destination)
        {
            return Err(LocalOperationFailure::needs_attention(
                "the reviewed destination changed during final verification",
            ));
        }
        CopyProvider::remove_source(
            &mut local,
            review.source(),
            review.source_snapshot(),
            review.source_removal(),
        )
        .map_err(|error| {
            LocalOperationFailure::needs_attention(format!(
                "the destination was published but source removal needs inspection: {error}"
            ))
        })?;
        CommandTargetRef::new(completed.id().clone(), completed.path().clone())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))
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
        let _ = action;
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let outcome = runtime.block_on(self.execute_copy(execution))?;
        if execution.action() == DropAction::Copy {
            return Ok(outcome);
        }
        let TransferOutcome::Completed(target) = outcome else {
            return Err(LocalOperationFailure::needs_attention(
                "the remote move did not produce a completed destination",
            ));
        };
        let mut local = LocalStore::new();
        let snapshot = CopyProvider::inspect(&mut local, execution.source(), false)
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        let removal =
            CopyProvider::prepare_source_removal(&mut local, execution.source(), &snapshot)
                .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        let review = MoveMetadataReview::new(
            execution.source().clone(),
            execution.destination().clone(),
            snapshot,
            removal,
            CopyStrategy::Streamed,
            remote_move_metadata_loss(),
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

pub(crate) struct RemoteDownloadRoute {
    source: Arc<RemoteProfileStore>,
    destination_provider: ProviderId,
}

impl RemoteDownloadRoute {
    pub(crate) fn new(source: Arc<RemoteProfileStore>) -> Self {
        Self {
            source,
            destination_provider: ProviderId::new("local").expect("the local provider ID is valid"),
        }
    }

    async fn execute_copy(
        &self,
        execution: ProviderTransferExecution<'_>,
    ) -> Result<TransferOutcome, LocalOperationFailure> {
        let destination = execution.destination().as_unix_path().ok_or_else(|| {
            LocalOperationFailure::failed("the download destination is not a local path")
        })?;
        let cancellation = execution.cancellation().clone();
        cancellation
            .check()
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let remote = self
            .source
            .connect_transfer(cancellation.clone())
            .await
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let source = remote
            .resolve_item(execution.source())
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
            .ok_or_else(|| LocalOperationFailure::failed("the remote source disappeared"))?;
        if source.kind() != ItemKind::RegularFile {
            return Err(LocalOperationFailure::failed(
                "remote download currently requires a regular file",
            ));
        }
        if execution
            .expected_identity()
            .is_some_and(|expected| expected != source.id())
        {
            return Err(LocalOperationFailure::failed(
                "the remote source changed before the transfer",
            ));
        }
        let size = source
            .size()
            .ok_or_else(|| LocalOperationFailure::failed("the remote source size is unknown"))?;
        let local = LocalStore::new();
        let source_snapshot = ProviderSnapshot::new(
            self.source.provider_id().clone(),
            remote.capabilities(execution.source()),
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
        if local
            .resolve_item(execution.destination())
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
            .is_some()
        {
            return Err(LocalOperationFailure::failed(
                "the local destination already exists",
            ));
        }
        let parent = destination.parent().ok_or_else(|| {
            LocalOperationFailure::failed("the download destination has no parent")
        })?;
        let budget = local_staging_budget(parent)?;
        if size > budget {
            return Err(LocalOperationFailure::failed(
                "the remote file exceeds the available local staging budget",
            ));
        }
        let staging = StagingPath::for_destination_with_nonce(
            execution.destination(),
            execution.id(),
            execution.generation(),
            StagingPath::unique_nonce(),
        )
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        let staging_path = staging.path().as_unix_path().ok_or_else(|| {
            LocalOperationFailure::failed("the download staging path is not local")
        })?;
        let downloaded = remote
            .download_to_new_local(
                execution.source(),
                staging_path,
                budget,
                cancellation.clone(),
            )
            .await
            .map_err(|error| {
                let message = format!("remote download stopped: {error}");
                if local.resolve_item(staging.path()).ok().flatten().is_some() {
                    LocalOperationFailure::recoverable(message, staging.path().clone())
                } else {
                    LocalOperationFailure::failed(message)
                }
            })?;
        if downloaded != size {
            return Err(LocalOperationFailure::recoverable(
                "the downloaded size does not match the remote source",
                staging.path().clone(),
            ));
        }
        let current = remote.resolve_item(execution.source()).map_err(|error| {
            LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
        })?;
        if current.as_ref().is_none_or(|item| item.id() != source.id()) {
            return Err(LocalOperationFailure::recoverable(
                "the remote source changed during the download",
                staging.path().clone(),
            ));
        }
        if local
            .resolve_item(execution.destination())
            .map_err(|error| {
                LocalOperationFailure::recoverable(error.to_string(), staging.path().clone())
            })?
            .is_some()
        {
            return Err(LocalOperationFailure::recoverable(
                "the local destination appeared during the download",
                staging.path().clone(),
            ));
        }
        let mut local = local;
        CopyProvider::publish(
            &mut local,
            staging.path(),
            execution.destination(),
            &cancellation,
        )
        .map_err(|error| match error {
            ProviderError::PublishUnknown => LocalOperationFailure::needs_attention(
                "local publication may have completed; inspect the destination before retrying",
            ),
            _ => LocalOperationFailure::recoverable(
                format!("local publication failed: {error}"),
                staging.path().clone(),
            ),
        })?;
        let completed = local
            .resolve_item(execution.destination())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?
            .ok_or_else(|| {
                LocalOperationFailure::needs_attention("published local destination is missing")
            })?;
        let target = CommandTargetRef::new(completed.id().clone(), completed.path().clone())
            .map_err(|error| LocalOperationFailure::needs_attention(error.to_string()))?;
        Ok(TransferOutcome::Completed(target))
    }
}

impl fmt::Debug for RemoteDownloadRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDownloadRoute")
            .field("source_provider", self.source.provider_id())
            .finish()
    }
}

impl ProviderTransferRoute for RemoteDownloadRoute {
    fn source_provider_id(&self) -> &ProviderId {
        self.source.provider_id()
    }

    fn destination_provider_id(&self) -> &ProviderId {
        &self.destination_provider
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
        let (provider, key) = source
            .provider_key()
            .ok_or("the download source is not a remote path")?;
        if provider != self.source.provider_id() || !key.starts_with(b"/") {
            return Err("the download source belongs to another provider".into());
        }
        let name = key.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
        if name.is_empty() || name == b"." || name == b".." {
            return Err("the remote source has no safe file name".into());
        }
        let parent = target
            .as_unix_path()
            .ok_or("the download target is not a local directory")?;
        Ok(StorePath::from_unix_path(
            parent.join(OsStr::from_bytes(name)),
        ))
    }

    fn provider_snapshot(&self, location: &StorePath) -> ProviderSnapshot {
        ProviderSnapshot::new(
            self.destination_provider.clone(),
            Store::capabilities(&LocalStore::new(), location),
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

pub(super) fn local_staging_budget(parent: &Path) -> Result<u64, LocalOperationFailure> {
    let space =
        statvfs(parent).map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
    let available = space
        .blocks_available()
        .saturating_mul(space.fragment_size());
    Ok(MAX_LOCAL_STAGING_BYTES.min(available / 2))
}

pub(super) async fn publish_remote_verified(
    remote: &OpendalStore,
    staging: &StagingPath,
    destination: &StorePath,
    expected_size: u64,
    cancellation: CancellationToken,
) -> Result<TransferOutcome, LocalOperationFailure> {
    remote
        .publish_staging_noreplace(staging, destination, cancellation)
        .await
        .map_err(|error| {
            LocalOperationFailure::needs_attention_with_staging(
                format!("remote publication may have completed: {error}"),
                staging.path().clone(),
            )
        })?;
    let completed = remote
        .resolve_item(destination)
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
    if completed.size() != Some(expected_size) {
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

pub(super) fn remote_move_metadata_loss() -> MetadataReport {
    MetadataReport::with_skipped([
        MetadataKind::Timestamps,
        MetadataKind::Mode,
        MetadataKind::Ownership,
        MetadataKind::ExtendedAttributes,
        MetadataKind::AccessControlList,
        MetadataKind::SparseLayout,
        MetadataKind::HardLinkRelationship,
    ])
}
