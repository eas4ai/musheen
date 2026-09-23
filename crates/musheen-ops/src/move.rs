use crate::copy::provider_failure_kind;
use crate::{
    CopyProvider, CopyRequest, CopySession, CopyStrategy, FailureKind, MetadataReport,
    OperationFailure,
};
use musheen_core::CancellationToken;

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
    let prepared = provider
        .prepare_source_removal(request.source(), copied.source_snapshot())
        .map_err(|error| {
            OperationFailure::after_publish(provider_failure_kind(error), request.destination())
        })?;
    provider
        .remove_source(request.source(), copied.source_snapshot(), &prepared)
        .map_err(|error| {
            let unknown = error == crate::ProviderError::SourceRemovalUnknown;
            let partial = error == crate::ProviderError::SourcePartiallyRemoved;
            let kind = provider_failure_kind(error);
            if unknown {
                OperationFailure::after_source_removal_unknown(kind, request.destination())
            } else if partial {
                OperationFailure::after_partial_source_removal(kind, request.destination())
            } else {
                OperationFailure::after_publish(kind, request.destination())
            }
        })?;
    Ok(MoveOutcome {
        strategy: MoveStrategy::VerifiedCopy,
        copy_strategy: Some(copied.strategy()),
        metadata: copied.metadata().clone(),
    })
}
