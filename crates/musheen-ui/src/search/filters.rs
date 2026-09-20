use crate::views::DirectoryViewModel;
use musheen_core::{ItemId, ItemKind, SearchQuery, SearchQueryError, StoreItem};
use musheen_desktop::TagCatalog;
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DirectoryFilter {
    name: String,
    kind: Option<ItemKind>,
    minimum_size: Option<u64>,
    maximum_size: Option<u64>,
    modified_after: Option<i64>,
    modified_before: Option<i64>,
    query: Option<SearchQuery>,
    tagged_items: Option<HashSet<ItemId>>,
}

impl DirectoryFilter {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into().to_lowercase(),
            ..Self::default()
        }
    }

    pub fn from_query(query: SearchQuery) -> Result<Self, SearchQueryError> {
        if !query.content_terms().is_empty() {
            return Err(SearchQueryError::UnsupportedCriterion("content".into()));
        }
        if query.mime().is_some() {
            return Err(SearchQueryError::UnsupportedCriterion("mime".into()));
        }
        if query.glob().is_some() {
            return Err(SearchQueryError::UnsupportedCriterion("glob".into()));
        }
        if query.has_hidden_policy() {
            return Err(SearchQueryError::UnsupportedCriterion("hidden".into()));
        }
        if query.has_follow_links_policy() {
            return Err(SearchQueryError::UnsupportedCriterion(
                "follow-links".into(),
            ));
        }
        Ok(Self {
            query: Some(query),
            ..Self::default()
        })
    }

    #[must_use]
    pub const fn with_kind(mut self, kind: ItemKind) -> Self {
        self.kind = Some(kind);
        self
    }

    #[must_use]
    pub const fn with_size(mut self, minimum: Option<u64>, maximum: Option<u64>) -> Self {
        self.minimum_size = minimum;
        self.maximum_size = maximum;
        self
    }

    #[must_use]
    pub const fn with_modified(mut self, after: Option<i64>, before: Option<i64>) -> Self {
        self.modified_after = after;
        self.modified_before = before;
        self
    }

    #[must_use]
    pub fn with_catalog_tag(mut self, catalog: &TagCatalog, tag: &str) -> Self {
        self.tagged_items = Some(catalog.items_with_tag(tag).into_iter().collect());
        self
    }

    #[must_use]
    pub fn apply<'a>(&self, directory: &'a DirectoryViewModel) -> Vec<&'a StoreItem> {
        directory
            .visible_items()
            .into_iter()
            .filter(|item| self.matches(item))
            .collect()
    }

    fn matches(&self, item: &StoreItem) -> bool {
        if self
            .tagged_items
            .as_ref()
            .is_some_and(|items| !items.contains(item.id()))
        {
            return false;
        }
        if let Some(query) = &self.query {
            let name = item.display_name().as_str().to_lowercase();
            if !query
                .name_terms()
                .iter()
                .all(|term| name.contains(&term.to_lowercase()))
                || query.kind().is_some_and(|kind| item.kind() != kind)
                || query
                    .size()
                    .is_some_and(|range| item.size().is_none_or(|size| !range.matches(&size)))
                || query.modified().is_some_and(|range| {
                    item.modified_unix_seconds()
                        .is_none_or(|modified| !range.matches(&modified))
                })
            {
                return false;
            }
        }
        if !item
            .display_name()
            .as_str()
            .to_lowercase()
            .contains(&self.name)
        {
            return false;
        }
        if self.kind.is_some_and(|kind| item.kind() != kind) {
            return false;
        }
        if self
            .minimum_size
            .is_some_and(|minimum| item.size().is_none_or(|size| size < minimum))
            || self
                .maximum_size
                .is_some_and(|maximum| item.size().is_none_or(|size| size > maximum))
        {
            return false;
        }
        if self.modified_after.is_some_and(|after| {
            item.modified_unix_seconds()
                .is_none_or(|modified| modified < after)
        }) || self.modified_before.is_some_and(|before| {
            item.modified_unix_seconds()
                .is_none_or(|modified| modified > before)
        }) {
            return false;
        }
        true
    }
}
