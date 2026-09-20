use musheen_core::{CapabilityKind, CapabilityMatrix, CapabilityState, ItemId, StorePath};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::error::Error;
use std::fmt;

const MAX_TAG_BYTES: usize = 128;
const XATTR_NAME: &str = "user.musheen.tags";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TagRecord {
    item: ItemId,
    path_hint: StorePath,
    tags: BTreeSet<Box<str>>,
    orphaned: bool,
}

/// App-owned fallback tag metadata, keyed only by provider-scoped item identity.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TagCatalog {
    records: Vec<TagRecord>,
}

impl TagCatalog {
    pub fn assign(
        &mut self,
        item: &ItemId,
        path_hint: StorePath,
        tag: &str,
    ) -> Result<bool, TagError> {
        let tag = validated_tag(tag)?;
        let record = self.record_mut_or_insert(item, path_hint);
        record.orphaned = false;
        Ok(record.tags.insert(tag))
    }

    pub fn remove(&mut self, item: &ItemId, tag: &str) -> bool {
        let Some(record) = self.records.iter_mut().find(|record| &record.item == item) else {
            return false;
        };
        let removed = record.tags.remove(tag);
        if record.tags.is_empty() {
            self.records.retain(|record| &record.item != item);
        }
        removed
    }

    #[must_use]
    pub fn tags_for(&self, item: &ItemId) -> BTreeSet<Box<str>> {
        self.records
            .iter()
            .find(|record| &record.item == item)
            .map(|record| record.tags.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn tag_names(&self) -> BTreeSet<Box<str>> {
        self.records
            .iter()
            .filter(|record| !record.orphaned)
            .flat_map(|record| record.tags.iter().cloned())
            .collect()
    }

    #[must_use]
    pub fn items_with_tag(&self, tag: &str) -> BTreeSet<ItemId> {
        self.records
            .iter()
            .filter(|record| !record.orphaned && record.tags.contains(tag))
            .map(|record| record.item.clone())
            .collect()
    }

    #[must_use]
    pub fn path_hint(&self, item: &ItemId) -> Option<&StorePath> {
        self.records
            .iter()
            .find(|record| &record.item == item)
            .map(|record| &record.path_hint)
    }

    pub fn tracked_items(&self) -> impl Iterator<Item = (&ItemId, &StorePath, bool)> {
        self.records
            .iter()
            .map(|record| (&record.item, &record.path_hint, record.orphaned))
    }

    pub fn orphaned_items(
        &self,
    ) -> impl Iterator<Item = (&ItemId, &StorePath, &BTreeSet<Box<str>>)> {
        self.records
            .iter()
            .filter(|record| record.orphaned)
            .map(|record| (&record.item, &record.path_hint, &record.tags))
    }

    pub fn rename(&mut self, old: &str, new: &str) -> Result<usize, TagError> {
        let new = validated_tag(new)?;
        if old == new.as_ref() {
            return Ok(0);
        }
        let mut changed = 0;
        for record in &mut self.records {
            if record.tags.remove(old) {
                record.tags.insert(new.clone());
                changed += 1;
            }
        }
        Ok(changed)
    }

    pub fn delete(&mut self, tag: &str) -> usize {
        let mut changed = 0;
        for record in &mut self.records {
            if record.tags.remove(tag) {
                changed += 1;
            }
        }
        self.records.retain(|record| !record.tags.is_empty());
        changed
    }

    /// Records an application-controlled rename or move. Cross-provider moves
    /// transfer metadata only when the destination explicitly supports tags.
    pub fn note_app_move(
        &mut self,
        source: &ItemId,
        destination: ItemId,
        path_hint: StorePath,
        destination_supports_tags: bool,
    ) -> TagMoveOutcome {
        if !destination_supports_tags {
            return TagMoveOutcome::UnsupportedDestination;
        }
        let Some(source_index) = self
            .records
            .iter()
            .position(|record| &record.item == source)
        else {
            return TagMoveOutcome::Preserved;
        };
        if source == &destination {
            let record = &mut self.records[source_index];
            record.path_hint = path_hint;
            record.orphaned = false;
            return TagMoveOutcome::Preserved;
        }
        let source_record = self.records.remove(source_index);
        if let Some(destination_record) = self
            .records
            .iter_mut()
            .find(|record| record.item == destination)
        {
            destination_record.tags.extend(source_record.tags);
            destination_record.path_hint = path_hint;
            destination_record.orphaned = false;
        } else {
            self.records.push(TagRecord {
                item: destination,
                path_hint,
                tags: source_record.tags,
                orphaned: false,
            });
        }
        TagMoveOutcome::Preserved
    }

    pub fn observe_present(&mut self, item: &ItemId, path_hint: StorePath) {
        if let Some(record) = self.records.iter_mut().find(|record| &record.item == item) {
            record.path_hint = path_hint;
            record.orphaned = false;
        }
    }

    pub fn observe_missing(&mut self, item: &ItemId) {
        if let Some(record) = self.records.iter_mut().find(|record| &record.item == item) {
            record.orphaned = true;
        }
    }

    #[must_use]
    pub fn is_orphaned(&self, item: &ItemId) -> bool {
        self.records
            .iter()
            .find(|record| &record.item == item)
            .is_some_and(|record| record.orphaned)
    }

    /// Deletes only exact, reviewed identities which are already orphaned.
    /// Path hints are deliberately ignored.
    pub fn cleanup_reviewed_orphans<'a>(
        &mut self,
        reviewed: impl IntoIterator<Item = &'a ItemId>,
    ) -> usize {
        let reviewed = reviewed.into_iter().cloned().collect::<BTreeSet<_>>();
        let before = self.records.len();
        self.records
            .retain(|record| !record.orphaned || !reviewed.contains(&record.item));
        before - self.records.len()
    }

    fn record_mut_or_insert(&mut self, item: &ItemId, path_hint: StorePath) -> &mut TagRecord {
        if let Some(index) = self.records.iter().position(|record| &record.item == item) {
            let record = &mut self.records[index];
            record.path_hint = path_hint;
            return record;
        }
        self.records.push(TagRecord {
            item: item.clone(),
            path_hint,
            tags: BTreeSet::new(),
            orphaned: false,
        });
        self.records.last_mut().expect("a tag record was inserted")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagMoveOutcome {
    Preserved,
    UnsupportedDestination,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagError {
    Empty,
    TooLong,
}

impl fmt::Display for TagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "tag names must contain visible text",
            Self::TooLong => "tag names must not exceed 128 bytes",
        })
    }
}

impl Error for TagError {}

fn validated_tag(tag: &str) -> Result<Box<str>, TagError> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err(TagError::Empty);
    }
    if tag.len() > MAX_TAG_BYTES {
        return Err(TagError::TooLong);
    }
    Ok(tag.into())
}

/// Storage seam shared by extended-attribute and app-owned tag providers.
pub trait TagBackend {
    type Error: Error + Send + Sync + 'static;

    fn read_tags(
        &self,
        item: &ItemId,
        path_hint: &StorePath,
    ) -> Result<BTreeSet<Box<str>>, Self::Error>;

    fn write_tags(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        tags: &BTreeSet<Box<str>>,
    ) -> Result<(), Self::Error>;
}

/// One tag model used by every provider backend.
pub struct BackendTagService<B> {
    backend: B,
}

impl<B: TagBackend> BackendTagService<B> {
    #[must_use]
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn tags(
        &self,
        item: &ItemId,
        path_hint: &StorePath,
    ) -> Result<BTreeSet<Box<str>>, TagServiceError<B::Error>> {
        self.backend
            .read_tags(item, path_hint)
            .map_err(TagServiceError::Backend)
    }

    pub fn assign(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        tag: &str,
    ) -> Result<(), TagServiceError<B::Error>> {
        let tag = validated_tag(tag).map_err(TagServiceError::InvalidTag)?;
        let mut tags = self.tags(item, path_hint)?;
        tags.insert(tag);
        self.backend
            .write_tags(item, path_hint, &tags)
            .map_err(TagServiceError::Backend)
    }

    pub fn remove(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        tag: &str,
    ) -> Result<(), TagServiceError<B::Error>> {
        let mut tags = self.tags(item, path_hint)?;
        tags.remove(tag);
        self.backend
            .write_tags(item, path_hint, &tags)
            .map_err(TagServiceError::Backend)
    }

    #[must_use]
    pub fn into_backend(self) -> B {
        self.backend
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagStorage {
    ExtendedAttribute,
    AppCatalog,
}

/// The single production tag model. Callers supply the provider's live
/// capability matrix for each target; filesystem metadata is used only when
/// both the provider explicitly supports xattrs and the user opted in.
pub struct TagService<'a> {
    catalog: &'a mut TagCatalog,
    xattr_opt_in: bool,
}

impl<'a> TagService<'a> {
    #[must_use]
    pub const fn new(catalog: &'a mut TagCatalog, xattr_opt_in: bool) -> Self {
        Self {
            catalog,
            xattr_opt_in,
        }
    }

    pub fn set_xattr_opt_in(&mut self, enabled: bool) {
        self.xattr_opt_in = enabled;
    }

    pub fn validate_tag(tag: &str) -> Result<(), TagError> {
        validated_tag(tag).map(|_| ())
    }

    pub fn assign(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        capabilities: &CapabilityMatrix,
        tag: &str,
    ) -> Result<TagStorage, CatalogTagError> {
        let storage = self.storage(path_hint, capabilities)?;
        if storage == TagStorage::ExtendedAttribute {
            let tag = validated_tag(tag).map_err(CatalogTagError::InvalidTag)?;
            let mut backend = XattrTagBackend::new(true, true);
            let mut tags = backend
                .read_tags(item, path_hint)
                .map_err(|error| CatalogTagError::Xattr(TagServiceError::Backend(error)))?;
            tags.extend(self.catalog.tags_for(item));
            tags.insert(tag);
            backend
                .write_tags(item, path_hint, &tags)
                .map_err(|error| CatalogTagError::Xattr(TagServiceError::Backend(error)))?;
            self.catalog
                .write_tags(item, path_hint, &tags)
                .expect("the app-owned tag catalog is infallible");
            return Ok(storage);
        }
        self.catalog
            .assign(item, path_hint.clone(), tag)
            .map_err(CatalogTagError::InvalidTag)?;
        Ok(storage)
    }

    pub fn remove(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        capabilities: &CapabilityMatrix,
        tag: &str,
    ) -> Result<TagStorage, CatalogTagError> {
        let storage = self.storage(path_hint, capabilities)?;
        if storage == TagStorage::ExtendedAttribute {
            let mut backend = XattrTagBackend::new(true, true);
            let mut tags = backend
                .read_tags(item, path_hint)
                .map_err(|error| CatalogTagError::Xattr(TagServiceError::Backend(error)))?;
            tags.extend(self.catalog.tags_for(item));
            tags.remove(tag);
            backend
                .write_tags(item, path_hint, &tags)
                .map_err(|error| CatalogTagError::Xattr(TagServiceError::Backend(error)))?;
            self.catalog
                .write_tags(item, path_hint, &tags)
                .expect("the app-owned tag catalog is infallible");
            return Ok(storage);
        }
        self.catalog.remove(item, tag);
        Ok(storage)
    }

    pub fn tags(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        capabilities: &CapabilityMatrix,
    ) -> Result<BTreeSet<Box<str>>, CatalogTagError> {
        let storage = self.storage(path_hint, capabilities)?;
        if storage == TagStorage::AppCatalog {
            return Ok(self.catalog.tags_for(item));
        }
        let xattr_tags = BackendTagService::new(XattrTagBackend::new(true, true))
            .tags(item, path_hint)
            .map_err(CatalogTagError::Xattr)?;
        let mut tags = self.catalog.tags_for(item);
        tags.extend(xattr_tags.iter().cloned());
        if tags != xattr_tags {
            XattrTagBackend::new(true, true)
                .write_tags(item, path_hint, &tags)
                .map_err(|error| CatalogTagError::Xattr(TagServiceError::Backend(error)))?;
        }
        self.catalog
            .write_tags(item, path_hint, &tags)
            .expect("the app-owned tag catalog is infallible");
        Ok(tags)
    }

    #[must_use]
    pub fn tags_for(&self, item: &ItemId) -> BTreeSet<Box<str>> {
        self.catalog.tags_for(item)
    }

    #[must_use]
    pub fn tag_names(&self) -> BTreeSet<Box<str>> {
        self.catalog.tag_names()
    }

    #[must_use]
    pub fn items_with_tag(&self, tag: &str) -> BTreeSet<ItemId> {
        self.catalog.items_with_tag(tag)
    }

    pub fn tracked_items(&self) -> impl Iterator<Item = (&ItemId, &StorePath, bool)> {
        self.catalog.tracked_items()
    }

    pub fn orphaned_items(
        &self,
    ) -> impl Iterator<Item = (&ItemId, &StorePath, &BTreeSet<Box<str>>)> {
        self.catalog.orphaned_items()
    }

    pub fn observe_present(&mut self, item: &ItemId, path_hint: StorePath) {
        self.catalog.observe_present(item, path_hint);
    }

    pub fn observe_missing(&mut self, item: &ItemId) {
        self.catalog.observe_missing(item);
    }

    pub fn cleanup_reviewed_orphans<'b>(
        &mut self,
        reviewed: impl IntoIterator<Item = &'b ItemId>,
    ) -> usize {
        self.catalog.cleanup_reviewed_orphans(reviewed)
    }

    fn storage(
        &self,
        path_hint: &StorePath,
        capabilities: &CapabilityMatrix,
    ) -> Result<TagStorage, CatalogTagError> {
        if !matches!(
            capabilities.get(CapabilityKind::Tags),
            CapabilityState::Supported
        ) {
            return Err(CatalogTagError::UnsupportedProvider(
                capabilities
                    .get(CapabilityKind::Tags)
                    .reason()
                    .unwrap_or("the provider does not support tags")
                    .into(),
            ));
        }
        Ok(
            if self.xattr_opt_in
                && path_hint.as_unix_path().is_some()
                && matches!(
                    capabilities.get(CapabilityKind::ExtendedAttributes),
                    CapabilityState::Supported
                )
            {
                TagStorage::ExtendedAttribute
            } else {
                TagStorage::AppCatalog
            },
        )
    }
}

#[derive(Debug)]
pub enum CatalogTagError {
    InvalidTag(TagError),
    UnsupportedProvider(Box<str>),
    Xattr(TagServiceError<XattrTagError>),
}

impl fmt::Display for CatalogTagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTag(error) => error.fmt(formatter),
            Self::UnsupportedProvider(reason) => formatter.write_str(reason),
            Self::Xattr(error) => error.fmt(formatter),
        }
    }
}

impl Error for CatalogTagError {}

#[derive(Debug)]
pub enum TagServiceError<E> {
    InvalidTag(TagError),
    Backend(E),
}

impl<E: fmt::Display> fmt::Display for TagServiceError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTag(error) => error.fmt(formatter),
            Self::Backend(error) => write!(formatter, "tag storage failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for TagServiceError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidTag(error) => Some(error),
            Self::Backend(error) => Some(error),
        }
    }
}

impl TagBackend for TagCatalog {
    type Error = Infallible;

    fn read_tags(
        &self,
        item: &ItemId,
        _path_hint: &StorePath,
    ) -> Result<BTreeSet<Box<str>>, Self::Error> {
        Ok(self.tags_for(item))
    }

    fn write_tags(
        &mut self,
        item: &ItemId,
        path_hint: &StorePath,
        tags: &BTreeSet<Box<str>>,
    ) -> Result<(), Self::Error> {
        self.records.retain(|record| &record.item != item);
        if !tags.is_empty() {
            self.records.push(TagRecord {
                item: item.clone(),
                path_hint: path_hint.clone(),
                tags: tags.clone(),
                orphaned: false,
            });
        }
        Ok(())
    }
}

/// Opt-in local xattr backend. Both capability support and user consent are
/// required before any metadata is written into the user's filesystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XattrTagBackend {
    enabled: bool,
}

impl XattrTagBackend {
    #[must_use]
    pub const fn new(provider_supports_tags: bool, user_enabled: bool) -> Self {
        Self {
            enabled: provider_supports_tags && user_enabled,
        }
    }

    fn require_enabled(self) -> Result<(), XattrTagError> {
        self.enabled
            .then_some(())
            .ok_or(XattrTagError::MetadataDisabled)
    }
}

impl TagBackend for XattrTagBackend {
    type Error = XattrTagError;

    fn read_tags(
        &self,
        _item: &ItemId,
        path_hint: &StorePath,
    ) -> Result<BTreeSet<Box<str>>, Self::Error> {
        self.require_enabled()?;
        let path = path_hint
            .as_unix_path()
            .ok_or(XattrTagError::NonLocalPath)?;
        let Some(bytes) = xattr::get(path, XATTR_NAME).map_err(XattrTagError::Io)? else {
            return Ok(BTreeSet::new());
        };
        serde_json::from_slice(&bytes).map_err(XattrTagError::Document)
    }

    fn write_tags(
        &mut self,
        _item: &ItemId,
        path_hint: &StorePath,
        tags: &BTreeSet<Box<str>>,
    ) -> Result<(), Self::Error> {
        self.require_enabled()?;
        let path = path_hint
            .as_unix_path()
            .ok_or(XattrTagError::NonLocalPath)?;
        if tags.is_empty() {
            match xattr::remove(path, XATTR_NAME) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(XattrTagError::Io(error)),
            }
        } else {
            let bytes = serde_json::to_vec(tags).map_err(XattrTagError::Document)?;
            xattr::set(path, XATTR_NAME, &bytes).map_err(XattrTagError::Io)
        }
    }
}

#[derive(Debug)]
pub enum XattrTagError {
    MetadataDisabled,
    NonLocalPath,
    Io(std::io::Error),
    Document(serde_json::Error),
}

impl fmt::Display for XattrTagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MetadataDisabled => formatter.write_str(
                "extended-attribute tags require provider support and explicit user consent",
            ),
            Self::NonLocalPath => {
                formatter.write_str("extended-attribute tags require a local path")
            }
            Self::Io(error) => write!(formatter, "extended-attribute access failed: {error}"),
            Self::Document(error) => {
                write!(formatter, "extended-attribute tag data is invalid: {error}")
            }
        }
    }
}

impl Error for XattrTagError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::MetadataDisabled | Self::NonLocalPath => None,
        }
    }
}
