use crate::MutationError;
use crate::create::validate_operation_path;
use musheen_core::StorePath;
use std::collections::HashSet;

pub trait DeleteProvider {
    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError>;

    fn supports_trash(&mut self, path: &StorePath) -> Result<bool, MutationError>;

    fn supports_permanent_delete(&mut self, path: &StorePath) -> Result<bool, MutationError>;

    /// Moves one item to trash after revalidating its identity at the mutation boundary.
    fn move_to_trash(&mut self, target: &DeleteTarget) -> Result<TrashReceipt, MutationError>;

    fn restore_no_replace(&mut self, receipt: &TrashReceipt) -> Result<(), MutationError>;

    /// The sole provider boundary for irreversible removal.
    fn permanently_delete(&mut self, target: &DeleteTarget) -> Result<(), MutationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteTarget {
    path: StorePath,
    expected_identity: Box<[u8]>,
}

impl DeleteTarget {
    #[must_use]
    pub fn new(path: StorePath, expected_identity: Vec<u8>) -> Self {
        Self {
            path,
            expected_identity: expected_identity.into_boxed_slice(),
        }
    }

    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    #[must_use]
    pub const fn expected_identity(&self) -> &[u8] {
        &self.expected_identity
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrashReceipt {
    original_path: StorePath,
    provider_reference: Box<[u8]>,
}

impl TrashReceipt {
    #[must_use]
    pub fn new(original_path: StorePath, provider_reference: Vec<u8>) -> Self {
        Self {
            original_path,
            provider_reference: provider_reference.into_boxed_slice(),
        }
    }

    #[must_use]
    pub const fn original_path(&self) -> &StorePath {
        &self.original_path
    }

    #[must_use]
    pub const fn provider_reference(&self) -> &[u8] {
        &self.provider_reference
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteFailure {
    target: DeleteTarget,
    error: MutationError,
}

impl DeleteFailure {
    #[must_use]
    pub const fn target(&self) -> &DeleteTarget {
        &self.target
    }

    #[must_use]
    pub const fn error(&self) -> &MutationError {
        &self.error
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeleteOutcome {
    trashed: Vec<TrashReceipt>,
    deleted: Vec<StorePath>,
    failures: Vec<DeleteFailure>,
}

impl DeleteOutcome {
    #[must_use]
    pub fn trashed(&self) -> &[TrashReceipt] {
        &self.trashed
    }

    #[must_use]
    pub fn deleted(&self) -> &[StorePath] {
        &self.deleted
    }

    #[must_use]
    pub fn failures(&self) -> &[DeleteFailure] {
        &self.failures
    }
}

pub fn execute_delete(
    provider: &mut impl DeleteProvider,
    targets: Vec<DeleteTarget>,
) -> Result<DeleteOutcome, MutationError> {
    validate_unique_targets(&targets)?;
    preflight(provider, &targets)?;
    for target in &targets {
        if !provider.supports_trash(target.path())? {
            return Err(MutationError::TrashUnsupported);
        }
    }

    let mut outcome = DeleteOutcome::default();
    for target in targets {
        match provider.move_to_trash(&target) {
            Ok(receipt) => outcome.trashed.push(receipt),
            Err(error) => outcome.failures.push(DeleteFailure { target, error }),
        }
    }
    Ok(outcome)
}

pub fn execute_restore(
    provider: &mut impl DeleteProvider,
    receipt: &TrashReceipt,
) -> Result<(), MutationError> {
    if provider.identity(receipt.original_path())?.is_some() {
        return Err(MutationError::Conflict);
    }
    provider.restore_no_replace(receipt)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermanentDeleteRequest {
    location: StorePath,
    targets: Vec<DeleteTarget>,
    digest: blake3::Hash,
}

impl PermanentDeleteRequest {
    pub fn new(location: StorePath, targets: Vec<DeleteTarget>) -> Result<Self, MutationError> {
        if targets.is_empty() || !targets_belong_to_location(&location, &targets) {
            return Err(MutationError::InvalidScope);
        }
        validate_unique_targets(&targets)?;
        let digest = scope_digest(&location, &targets)?;
        Ok(Self {
            location,
            targets,
            digest,
        })
    }

    #[must_use]
    pub fn challenge(&self) -> PermanentDeleteChallenge {
        PermanentDeleteChallenge {
            location: self.location.clone(),
            item_count: self.targets.len(),
            digest: self.digest,
        }
    }

    #[must_use]
    pub const fn location(&self) -> &StorePath {
        &self.location
    }

    #[must_use]
    pub fn targets(&self) -> &[DeleteTarget] {
        &self.targets
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermanentDeleteChallenge {
    location: StorePath,
    item_count: usize,
    digest: blake3::Hash,
}

impl PermanentDeleteChallenge {
    pub fn confirm(
        &self,
        item_count: usize,
        location: &StorePath,
        acknowledges_no_recovery: bool,
    ) -> Result<PermanentDeleteConfirmation, MutationError> {
        if item_count != self.item_count || location != &self.location || !acknowledges_no_recovery
        {
            return Err(MutationError::ConfirmationRequired);
        }
        Ok(PermanentDeleteConfirmation {
            digest: self.digest,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermanentDeleteConfirmation {
    digest: blake3::Hash,
}

pub fn execute_permanent_delete(
    provider: &mut impl DeleteProvider,
    request: &PermanentDeleteRequest,
    confirmation: &PermanentDeleteConfirmation,
) -> Result<DeleteOutcome, MutationError> {
    if confirmation.digest != request.digest {
        return Err(MutationError::ConfirmationRequired);
    }
    for target in &request.targets {
        if !provider.supports_permanent_delete(target.path())? {
            return Err(MutationError::Unsupported);
        }
    }
    preflight(provider, &request.targets)?;
    let mut outcome = DeleteOutcome::default();
    for target in &request.targets {
        match provider.permanently_delete(target) {
            Ok(()) => outcome.deleted.push(target.path.clone()),
            Err(error) => outcome.failures.push(DeleteFailure {
                target: target.clone(),
                error,
            }),
        }
    }
    Ok(outcome)
}

fn preflight(
    provider: &mut impl DeleteProvider,
    targets: &[DeleteTarget],
) -> Result<(), MutationError> {
    for target in targets {
        let Some(identity) = provider.identity(target.path())? else {
            return Err(MutationError::Missing);
        };
        if identity.as_ref() != target.expected_identity() {
            return Err(MutationError::SourceChanged);
        }
    }
    Ok(())
}

fn targets_belong_to_location(location: &StorePath, targets: &[DeleteTarget]) -> bool {
    let Some(location) = location.as_unix_path() else {
        return false;
    };
    if validate_operation_path(location).is_err() {
        return false;
    }
    targets.iter().all(|target| {
        target.path.as_unix_path().is_some_and(|path| {
            validate_operation_path(path).is_ok()
                && path.parent().is_some_and(|parent| parent == location)
        })
    })
}

fn validate_unique_targets(targets: &[DeleteTarget]) -> Result<(), MutationError> {
    if targets.is_empty() {
        return Err(MutationError::InvalidScope);
    }
    let mut paths = HashSet::with_capacity(targets.len());
    if targets.iter().all(|target| paths.insert(target.path())) {
        Ok(())
    } else {
        Err(MutationError::BatchCollision)
    }
}

fn scope_digest(
    location: &StorePath,
    targets: &[DeleteTarget],
) -> Result<blake3::Hash, MutationError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"musheen-permanent-delete-v1\0");
    let location = serde_json::to_vec(location)
        .map_err(|error| MutationError::Provider(error.to_string().into()))?;
    hasher.update(&location);
    for target in targets {
        let path = serde_json::to_vec(&target.path)
            .map_err(|error| MutationError::Provider(error.to_string().into()))?;
        hasher.update(&path);
        hasher.update(&target.expected_identity);
    }
    Ok(hasher.finalize())
}
