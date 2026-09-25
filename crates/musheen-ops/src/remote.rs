use crate::{OperationKind, ProviderSnapshot};
use musheen_core::{CapabilityKind, CapabilityState};
use std::error::Error;
use std::fmt;

/// Backend facts which cannot be inferred from the portable capability matrix.
/// A connection adapter must report only behavior it has actually established.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoteTransferCapabilities {
    read: bool,
    write: bool,
    server_copy: bool,
    server_move: bool,
    stable_identity: bool,
    range_read: bool,
    range_write: bool,
    atomic_publish: bool,
    crash_durable: bool,
}

impl RemoteTransferCapabilities {
    #[must_use]
    pub const fn readable() -> Self {
        Self {
            read: true,
            ..Self::default_const()
        }
    }

    #[must_use]
    pub const fn writable() -> Self {
        Self {
            write: true,
            ..Self::default_const()
        }
    }

    const fn default_const() -> Self {
        Self {
            read: false,
            write: false,
            server_copy: false,
            server_move: false,
            stable_identity: false,
            range_read: false,
            range_write: false,
            atomic_publish: false,
            crash_durable: false,
        }
    }

    #[must_use]
    pub const fn with_write(mut self) -> Self {
        self.write = true;
        self
    }

    #[must_use]
    pub const fn with_server_copy(mut self) -> Self {
        self.server_copy = true;
        self
    }

    #[must_use]
    pub const fn with_server_move(mut self) -> Self {
        self.server_move = true;
        self
    }

    #[must_use]
    pub const fn with_stable_identity(mut self) -> Self {
        self.stable_identity = true;
        self
    }

    #[must_use]
    pub const fn with_range_read(mut self) -> Self {
        self.range_read = true;
        self
    }

    #[must_use]
    pub const fn with_range_write(mut self) -> Self {
        self.range_write = true;
        self
    }

    #[must_use]
    pub const fn with_atomic_publish(mut self) -> Self {
        self.atomic_publish = true;
        self
    }

    #[must_use]
    pub const fn with_crash_durability(mut self) -> Self {
        self.crash_durable = true;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteTransferStrategy {
    ServerSideCopy,
    ServerSideMove,
    Streamed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumePolicy {
    /// Start a new staging item after a failure; never append to unverified data.
    RestartOnly,
    /// An adapter may resume only after rechecking identities and the written prefix.
    VerifiedRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteTransferGap {
    AtomicPublication,
    CrashDurability,
    Permissions,
    Ownership,
    ExtendedAttributes,
    SparseLayout,
    SymbolicLinks,
    HardLinks,
    Trash,
}

impl RemoteTransferGap {
    const fn requires_metadata_review(self) -> bool {
        matches!(
            self,
            Self::Permissions
                | Self::Ownership
                | Self::ExtendedAttributes
                | Self::SparseLayout
                | Self::SymbolicLinks
                | Self::HardLinks
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteTransferPlanError {
    UnsupportedOperation,
    SourceUnreadable,
    DestinationUnwritable,
}

impl fmt::Display for RemoteTransferPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOperation => {
                formatter.write_str("only copy and move can be transferred")
            }
            Self::SourceUnreadable => formatter.write_str("the source cannot be read for transfer"),
            Self::DestinationUnwritable => {
                formatter.write_str("the destination cannot be written for transfer")
            }
        }
    }
}

impl Error for RemoteTransferPlanError {}

/// A two-provider preflight result. It describes permitted execution and lost
/// guarantees; it is not itself proof that a specific remote mutation succeeded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteTransferPlan {
    strategy: RemoteTransferStrategy,
    resume_policy: ResumePolicy,
    gaps: Vec<RemoteTransferGap>,
}

impl RemoteTransferPlan {
    pub fn new(
        operation: OperationKind,
        source: &ProviderSnapshot,
        destination: &ProviderSnapshot,
        source_io: RemoteTransferCapabilities,
        destination_io: RemoteTransferCapabilities,
    ) -> Result<Self, RemoteTransferPlanError> {
        let strategy = select_strategy(operation, source, destination, source_io, destination_io)?;
        let resume_policy = if strategy == RemoteTransferStrategy::Streamed
            && source_io.stable_identity
            && source_io.range_read
            && destination_io.stable_identity
            && destination_io.range_write
        {
            ResumePolicy::VerifiedRange
        } else {
            ResumePolicy::RestartOnly
        };
        Ok(Self {
            strategy,
            resume_policy,
            gaps: transfer_gaps(source, destination, destination_io),
        })
    }

    #[must_use]
    pub const fn strategy(&self) -> RemoteTransferStrategy {
        self.strategy
    }

    #[must_use]
    pub const fn resume_policy(&self) -> ResumePolicy {
        self.resume_policy
    }

    #[must_use]
    pub fn gaps(&self) -> &[RemoteTransferGap] {
        &self.gaps
    }

    #[must_use]
    pub fn requires_metadata_review_before_source_removal(&self) -> bool {
        self.gaps
            .iter()
            .copied()
            .any(RemoteTransferGap::requires_metadata_review)
    }
}

fn select_strategy(
    operation: OperationKind,
    source: &ProviderSnapshot,
    destination: &ProviderSnapshot,
    source_io: RemoteTransferCapabilities,
    destination_io: RemoteTransferCapabilities,
) -> Result<RemoteTransferStrategy, RemoteTransferPlanError> {
    let same_provider = source.id() == destination.id();
    match operation {
        OperationKind::Copy
            if same_provider && source_io.server_copy && destination_io.server_copy =>
        {
            Ok(RemoteTransferStrategy::ServerSideCopy)
        }
        OperationKind::Move
            if same_provider
                && source_io.server_move
                && destination_io.server_move
                && supported(source, CapabilityKind::AtomicRename)
                && supported(destination, CapabilityKind::AtomicRename) =>
        {
            Ok(RemoteTransferStrategy::ServerSideMove)
        }
        OperationKind::Copy | OperationKind::Move => {
            if !source_io.read {
                return Err(RemoteTransferPlanError::SourceUnreadable);
            }
            if !destination_io.write {
                return Err(RemoteTransferPlanError::DestinationUnwritable);
            }
            Ok(RemoteTransferStrategy::Streamed)
        }
        _ => Err(RemoteTransferPlanError::UnsupportedOperation),
    }
}

fn transfer_gaps(
    source: &ProviderSnapshot,
    destination: &ProviderSnapshot,
    destination_io: RemoteTransferCapabilities,
) -> Vec<RemoteTransferGap> {
    let mut gaps = Vec::new();
    if !destination_io.atomic_publish || !supported(destination, CapabilityKind::AtomicRename) {
        gaps.push(RemoteTransferGap::AtomicPublication);
    }
    if !destination_io.crash_durable {
        gaps.push(RemoteTransferGap::CrashDurability);
    }
    for (capability, gap) in [
        (CapabilityKind::Permissions, RemoteTransferGap::Permissions),
        (CapabilityKind::Ownership, RemoteTransferGap::Ownership),
        (
            CapabilityKind::ExtendedAttributes,
            RemoteTransferGap::ExtendedAttributes,
        ),
        (CapabilityKind::SparseFiles, RemoteTransferGap::SparseLayout),
        (
            CapabilityKind::SymbolicLinks,
            RemoteTransferGap::SymbolicLinks,
        ),
        (CapabilityKind::HardLinks, RemoteTransferGap::HardLinks),
        (CapabilityKind::Trash, RemoteTransferGap::Trash),
    ] {
        if supported(source, capability) && !supported(destination, capability) {
            gaps.push(gap);
        }
    }
    gaps
}

fn supported(provider: &ProviderSnapshot, capability: CapabilityKind) -> bool {
    matches!(
        provider.capabilities().get(capability),
        CapabilityState::Supported
    )
}
