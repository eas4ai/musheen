use crate::CoreError;

/// Every portable storage feature that providers must report explicitly.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum CapabilityKind {
    Permissions,
    Ownership,
    SymbolicLinks,
    HardLinks,
    SparseFiles,
    ExtendedAttributes,
    ReflinkCopies,
    Trash,
    AtomicRename,
    Watching,
    CaseSensitivity,
}

impl CapabilityKind {
    pub const ALL: [Self; 11] = [
        Self::Permissions,
        Self::Ownership,
        Self::SymbolicLinks,
        Self::HardLinks,
        Self::SparseFiles,
        Self::ExtendedAttributes,
        Self::ReflinkCopies,
        Self::Trash,
        Self::AtomicRename,
        Self::Watching,
        Self::CaseSensitivity,
    ];
}

/// A validated explanation for an unsupported or unknown capability.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CapabilityReason(Box<str>);

impl CapabilityReason {
    pub fn new(reason: impl Into<Box<str>>) -> Result<Self, CoreError> {
        let reason = reason.into();
        (!reason.trim().is_empty())
            .then_some(Self(reason))
            .ok_or(CoreError::InvalidCapabilityReason)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityState {
    Supported,
    Unsupported(CapabilityReason),
    Unknown(CapabilityReason),
}

impl CapabilityState {
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Supported => None,
            Self::Unsupported(reason) | Self::Unknown(reason) => Some(reason.as_str()),
        }
    }
}

/// A total capability map: construction always supplies one state per kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityMatrix([CapabilityState; CapabilityKind::ALL.len()]);

impl CapabilityMatrix {
    #[must_use]
    pub fn new(mut state_for: impl FnMut(CapabilityKind) -> CapabilityState) -> Self {
        Self(std::array::from_fn(|index| {
            state_for(CapabilityKind::ALL[index])
        }))
    }

    #[must_use]
    pub fn get(&self, kind: CapabilityKind) -> &CapabilityState {
        &self.0[kind as usize]
    }
}
