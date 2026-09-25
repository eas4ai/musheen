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
    partially_skipped: Vec<MetadataKind>,
    verification_skipped: Vec<MetadataKind>,
}

impl MetadataReport {
    #[must_use]
    pub fn with_skipped(kinds: impl IntoIterator<Item = MetadataKind>) -> Self {
        let mut skipped = kinds.into_iter().collect::<Vec<_>>();
        skipped.sort_unstable();
        skipped.dedup();
        Self {
            verification_skipped: skipped.clone(),
            skipped,
            partially_skipped: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_partially_skipped(mut self, kinds: impl IntoIterator<Item = MetadataKind>) -> Self {
        for kind in kinds {
            insert_kind(&mut self.partially_skipped, kind);
        }
        self
    }

    #[must_use]
    pub fn skipped(&self) -> &[MetadataKind] {
        &self.skipped
    }

    #[must_use]
    pub fn partially_skipped(&self) -> &[MetadataKind] {
        &self.partially_skipped
    }

    #[must_use]
    pub fn verification_skipped(&self) -> &[MetadataKind] {
        &self.verification_skipped
    }

    pub(crate) fn note_skipped(&mut self, kind: MetadataKind) {
        insert_kind(&mut self.skipped, kind);
        insert_kind(&mut self.verification_skipped, kind);
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.skipped.is_empty()
    }
}

fn insert_kind(kinds: &mut Vec<MetadataKind>, kind: MetadataKind) {
    match kinds.binary_search(&kind) {
        Ok(_) => {}
        Err(index) => kinds.insert(index, kind),
    }
}
