use crate::copy::provider_failure_kind;
use crate::{
    CopyProvider, CopyRequest, CopySession, CopyStrategy, EntrySnapshot, FailureKind,
    MetadataReport, OperationFailure,
};
use musheen_core::{CancellationToken, StorePath};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveStrategy {
    AtomicRename,
    VerifiedCopy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveOutcome {
    strategy: MoveStrategy,
    copy_strategy: Option<CopyStrategy>,
    metadata: MetadataReport,
    metadata_review: Option<MoveMetadataReview>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveMetadataReview {
    source: StorePath,
    destination: StorePath,
    source_snapshot: EntrySnapshot,
    copy_strategy: CopyStrategy,
    metadata: MetadataReport,
}

impl MoveMetadataReview {
    #[must_use]
    pub fn new(
        source: StorePath,
        destination: StorePath,
        source_snapshot: EntrySnapshot,
        copy_strategy: CopyStrategy,
        metadata: MetadataReport,
    ) -> Self {
        Self {
            source,
            destination,
            source_snapshot,
            copy_strategy,
            metadata,
        }
    }

    #[must_use]
    pub const fn source(&self) -> &StorePath {
        &self.source
    }

    #[must_use]
    pub const fn destination(&self) -> &StorePath {
        &self.destination
    }

    #[must_use]
    pub const fn metadata(&self) -> &MetadataReport {
        &self.metadata
    }
}

impl MoveOutcome {
    #[must_use]
    pub const fn strategy(&self) -> MoveStrategy {
        self.strategy
    }

    #[must_use]
    pub const fn copy_strategy(&self) -> Option<CopyStrategy> {
        self.copy_strategy
    }

    #[must_use]
    pub const fn metadata(&self) -> &MetadataReport {
        &self.metadata
    }

    #[must_use]
    pub const fn metadata_review(&self) -> Option<&MoveMetadataReview> {
        self.metadata_review.as_ref()
    }

    #[must_use]
    pub fn into_metadata_review(self) -> Option<MoveMetadataReview> {
        self.metadata_review
    }
}

pub fn execute_move<P: CopyProvider>(
    provider: &mut P,
    request: &CopyRequest,
    cancellation: &CancellationToken,
) -> Result<MoveOutcome, OperationFailure> {
    if cancellation.is_cancelled() {
        return Err(OperationFailure::before_publish(
            FailureKind::Cancelled,
            request.destination(),
        ));
    }
    let capabilities = provider.capabilities(request.source(), request.destination());
    if capabilities.atomic_rename {
        match provider.try_atomic_move(request.source(), request.destination()) {
            Ok(true) => {
                return Ok(MoveOutcome {
                    strategy: MoveStrategy::AtomicRename,
                    copy_strategy: None,
                    metadata: MetadataReport::default(),
                    metadata_review: None,
                });
            }
            Ok(false) => {}
            Err(error) => {
                let unknown = error == crate::ProviderError::AtomicMoveUnknown;
                let kind = provider_failure_kind(error);
                return Err(if unknown {
                    OperationFailure::atomic_move_unknown(kind, request.destination())
                } else {
                    OperationFailure::before_publish(kind, request.destination())
                });
            }
        }
    }

    let copied = CopySession::default().execute(provider, request, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(OperationFailure::after_publish(
            FailureKind::Cancelled,
            request.destination(),
        ));
    }
    let metadata = copied.metadata().clone();
    let copy_strategy = copied.strategy();
    if !metadata.complete() {
        return Ok(MoveOutcome {
            strategy: MoveStrategy::VerifiedCopy,
            copy_strategy: Some(copy_strategy),
            metadata: metadata.clone(),
            metadata_review: Some(MoveMetadataReview::new(
                request.source().clone(),
                request.destination().clone(),
                copied.source_snapshot().clone(),
                copy_strategy,
                metadata,
            )),
        });
    }
    remove_verified_source(
        provider,
        request.source(),
        request.destination(),
        copied.source_snapshot(),
    )?;
    Ok(MoveOutcome {
        strategy: MoveStrategy::VerifiedCopy,
        copy_strategy: Some(copy_strategy),
        metadata,
        metadata_review: None,
    })
}

pub fn complete_move_after_metadata_review<P: CopyProvider>(
    provider: &mut P,
    review: MoveMetadataReview,
    cancellation: &CancellationToken,
) -> Result<MoveOutcome, OperationFailure> {
    if cancellation.is_cancelled() {
        return Err(OperationFailure::after_publish(
            FailureKind::Cancelled,
            review.destination(),
        ));
    }
    let destination_is_still_verified = provider
        .verify(
            review.source(),
            &review.source_snapshot,
            review.destination(),
            review.metadata(),
        )
        .map_err(|error| {
            OperationFailure::after_publish(provider_failure_kind(error), review.destination())
        })?;
    if !destination_is_still_verified {
        return Err(OperationFailure::after_publish(
            FailureKind::VerificationFailed,
            review.destination(),
        ));
    }
    if cancellation.is_cancelled() {
        return Err(OperationFailure::after_publish(
            FailureKind::Cancelled,
            review.destination(),
        ));
    }
    remove_verified_source(
        provider,
        review.source(),
        review.destination(),
        &review.source_snapshot,
    )?;
    Ok(MoveOutcome {
        strategy: MoveStrategy::VerifiedCopy,
        copy_strategy: Some(review.copy_strategy),
        metadata: review.metadata,
        metadata_review: None,
    })
}

fn remove_verified_source<P: CopyProvider>(
    provider: &mut P,
    source: &StorePath,
    destination: &StorePath,
    source_snapshot: &EntrySnapshot,
) -> Result<(), OperationFailure> {
    let prepared = provider
        .prepare_source_removal(source, source_snapshot)
        .map_err(|error| {
            OperationFailure::after_publish(provider_failure_kind(error), destination)
        })?;
    provider
        .remove_source(source, source_snapshot, &prepared)
        .map_err(|error| {
            let unknown = error == crate::ProviderError::SourceRemovalUnknown;
            let partial = error == crate::ProviderError::SourcePartiallyRemoved;
            let kind = provider_failure_kind(error);
            if unknown {
                OperationFailure::after_source_removal_unknown(kind, destination)
            } else if partial {
                OperationFailure::after_partial_source_removal(kind, destination)
            } else {
                OperationFailure::after_publish(kind, destination)
            }
        })
}
