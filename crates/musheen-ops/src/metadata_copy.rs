#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MetadataKind {
    Timestamps,
    Mode,
    Ownership,
    ExtendedAttributes,
    AccessControlList,
    SparseLayout,
    HardLinkRelationship,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataReport {
    skipped: Vec<MetadataKind>,
}

impl MetadataReport {
    #[must_use]
    pub fn with_skipped(kinds: impl IntoIterator<Item = MetadataKind>) -> Self {
        let mut skipped = kinds.into_iter().collect::<Vec<_>>();
        skipped.sort_unstable();
        skipped.dedup();
        Self { skipped }
    }

    #[must_use]
    pub fn skipped(&self) -> &[MetadataKind] {
        &self.skipped
    }

    pub(crate) fn note_skipped(&mut self, kind: MetadataKind) {
        match self.skipped.binary_search(&kind) {
            Ok(_) => {}
            Err(index) => self.skipped.insert(index, kind),
        }
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.skipped.is_empty()
    }
}
