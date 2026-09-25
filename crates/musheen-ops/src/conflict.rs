use crate::{MutationError, OperationKind};
use musheen_core::StorePath;
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConflictItemKind {
    File,
    Directory,
    SymbolicLink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictChoice {
    Replace,
    Skip,
    KeepBoth,
    MergeDirectory,
    ReplaceTree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyScope {
    ThisConflict,
    CompatibleRemaining,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictRecord {
    operation: OperationKind,
    source: StorePath,
    source_identity: Box<[u8]>,
    source_kind: ConflictItemKind,
    destination: StorePath,
    destination_identity: Box<[u8]>,
    destination_kind: ConflictItemKind,
}

impl ConflictRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation: OperationKind,
        source: StorePath,
        source_identity: Vec<u8>,
        source_kind: ConflictItemKind,
        destination: StorePath,
        destination_identity: Vec<u8>,
        destination_kind: ConflictItemKind,
    ) -> Result<Self, ConflictError> {
        if source_identity.is_empty()
            || destination_identity.is_empty()
            || source == destination
            || !matches!(
                operation,
                OperationKind::Copy
                    | OperationKind::Move
                    | OperationKind::Extract
                    | OperationKind::Restore
                    | OperationKind::Rename
            )
        {
            return Err(ConflictError::InvalidRecord);
        }
        Ok(Self {
            operation,
            source,
            source_identity: source_identity.into_boxed_slice(),
            source_kind,
            destination,
            destination_identity: destination_identity.into_boxed_slice(),
            destination_kind,
        })
    }

    #[must_use]
    pub const fn operation(&self) -> OperationKind {
        self.operation
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
    pub const fn source_identity(&self) -> &[u8] {
        &self.source_identity
    }

    #[must_use]
    pub const fn destination_identity(&self) -> &[u8] {
        &self.destination_identity
    }

    #[must_use]
    pub const fn source_kind(&self) -> ConflictItemKind {
        self.source_kind
    }

    #[must_use]
    pub const fn destination_kind(&self) -> ConflictItemKind {
        self.destination_kind
    }

    fn accepts(&self, choice: ConflictChoice) -> bool {
        match choice {
            ConflictChoice::Skip | ConflictChoice::KeepBoth => true,
            ConflictChoice::Replace => {
                self.source_kind != ConflictItemKind::Directory
                    && self.destination_kind != ConflictItemKind::Directory
            }
            ConflictChoice::MergeDirectory | ConflictChoice::ReplaceTree => {
                self.source_kind == ConflictItemKind::Directory
                    && self.destination_kind == ConflictItemKind::Directory
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictDecision {
    operation: OperationKind,
    source: StorePath,
    source_identity: Box<[u8]>,
    destination: StorePath,
    destination_identity: Box<[u8]>,
    choice: ConflictChoice,
}

impl ConflictDecision {
    fn bind(conflict: &ConflictRecord, choice: ConflictChoice) -> Self {
        Self {
            operation: conflict.operation,
            source: conflict.source.clone(),
            source_identity: conflict.source_identity.clone(),
            destination: conflict.destination.clone(),
            destination_identity: conflict.destination_identity.clone(),
            choice,
        }
    }

    #[must_use]
    pub const fn choice(&self) -> ConflictChoice {
        self.choice
    }

    #[must_use]
    pub const fn operation(&self) -> OperationKind {
        self.operation
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
    pub const fn source_identity(&self) -> &[u8] {
        &self.source_identity
    }

    #[must_use]
    pub const fn destination_identity(&self) -> &[u8] {
        &self.destination_identity
    }
}

pub trait ConflictDecisionJournal {
    /// Persists the exact identities and choice before the operation resumes.
    fn persist_decision(&mut self, decision: &ConflictDecision) -> Result<(), MutationError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConflictPolicy {
    operation: OperationKind,
    source_kind: ConflictItemKind,
    destination_kind: ConflictItemKind,
    choice: ConflictChoice,
}

#[derive(Debug, Default)]
pub struct ConflictPolicies {
    compatible_remaining: Option<ConflictPolicy>,
}

impl ConflictPolicies {
    pub fn decide(
        &mut self,
        conflict: &ConflictRecord,
        choice: ConflictChoice,
        scope: ApplyScope,
        journal: &mut impl ConflictDecisionJournal,
    ) -> Result<ConflictDecision, ConflictError> {
        if !conflict.accepts(choice) {
            return Err(ConflictError::InvalidChoice);
        }
        let decision = ConflictDecision::bind(conflict, choice);
        journal.persist_decision(&decision)?;
        if scope == ApplyScope::CompatibleRemaining {
            self.compatible_remaining = Some(ConflictPolicy {
                operation: conflict.operation,
                source_kind: conflict.source_kind,
                destination_kind: conflict.destination_kind,
                choice,
            });
        }
        Ok(decision)
    }

    pub fn resolve_saved(
        &self,
        conflict: &ConflictRecord,
        current_source_identity: &[u8],
        current_destination_identity: &[u8],
        journal: &mut impl ConflictDecisionJournal,
    ) -> Result<Option<ConflictChoice>, ConflictError> {
        self.resolve_saved_decision(
            conflict,
            current_source_identity,
            current_destination_identity,
            journal,
        )
        .map(|decision| decision.map(|decision| decision.choice()))
    }

    pub fn resolve_saved_decision(
        &self,
        conflict: &ConflictRecord,
        current_source_identity: &[u8],
        current_destination_identity: &[u8],
        journal: &mut impl ConflictDecisionJournal,
    ) -> Result<Option<ConflictDecision>, ConflictError> {
        let Some(policy) = self.compatible_remaining else {
            return Ok(None);
        };
        if policy.operation != conflict.operation
            || policy.source_kind != conflict.source_kind
            || policy.destination_kind != conflict.destination_kind
        {
            return Ok(None);
        }
        if current_source_identity != conflict.source_identity() {
            return Err(ConflictError::StaleSource);
        }
        if current_destination_identity != conflict.destination_identity() {
            return Err(ConflictError::StaleDestination);
        }
        if !conflict.accepts(policy.choice) {
            return Err(ConflictError::InvalidChoice);
        }
        let decision = ConflictDecision::bind(conflict, policy.choice);
        journal.persist_decision(&decision)?;
        Ok(Some(decision))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConflictError {
    InvalidRecord,
    InvalidChoice,
    StaleSource,
    StaleDestination,
    Journal(MutationError),
}

impl fmt::Display for ConflictError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRecord => formatter.write_str("the conflict record is invalid"),
            Self::InvalidChoice => {
                formatter.write_str("the choice does not apply to this conflict")
            }
            Self::StaleSource => formatter.write_str("the conflict source changed"),
            Self::StaleDestination => formatter.write_str("the conflict destination changed"),
            Self::Journal(error) => {
                write!(formatter, "the conflict decision was not saved: {error}")
            }
        }
    }
}

impl Error for ConflictError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Journal(error) => Some(error),
            Self::InvalidRecord
            | Self::InvalidChoice
            | Self::StaleSource
            | Self::StaleDestination => None,
        }
    }
}

impl From<MutationError> for ConflictError {
    fn from(error: MutationError) -> Self {
        Self::Journal(error)
    }
}
