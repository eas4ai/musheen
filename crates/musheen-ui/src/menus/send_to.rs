use musheen_core::StorePath;

/// A destination supplied by the catalog, removable media, or a remote provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendToDestination {
    label: Box<str>,
    path: StorePath,
    writable: bool,
    kind: SendToDestinationKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendToDestinationKind {
    Pinned,
    Removable,
    Remote,
}

impl SendToDestination {
    #[must_use]
    pub fn pinned(label: impl Into<Box<str>>, path: StorePath, writable: bool) -> Self {
        Self::new(label, path, writable, SendToDestinationKind::Pinned)
    }

    #[must_use]
    pub fn removable(label: impl Into<Box<str>>, path: StorePath, writable: bool) -> Self {
        Self::new(label, path, writable, SendToDestinationKind::Removable)
    }

    #[must_use]
    pub fn remote(label: impl Into<Box<str>>, path: StorePath, writable: bool) -> Self {
        Self::new(label, path, writable, SendToDestinationKind::Remote)
    }

    #[must_use]
    fn new(
        label: impl Into<Box<str>>,
        path: StorePath,
        writable: bool,
        kind: SendToDestinationKind,
    ) -> Self {
        Self {
            label: label.into(),
            path,
            writable,
            kind,
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    #[must_use]
    pub const fn writable(&self) -> bool {
        self.writable
    }

    #[must_use]
    pub const fn kind(&self) -> SendToDestinationKind {
        self.kind
    }
}
