use crate::CoreError;
use std::num::NonZeroUsize;

/// Mutable settings input used to create an immutable operation snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceLimitConfig {
    pub directory_page_items: usize,
    pub directory_prefetch_pages: usize,
    pub directory_retained_items: usize,
    pub directory_rendered_viewports: usize,
}

impl ResourceLimitConfig {
    pub const MAX_DIRECTORY_PAGE_ITEMS: usize = 4_096;
    pub const MAX_DIRECTORY_PREFETCH_PAGES: usize = 8;
    pub const MAX_DIRECTORY_RETAINED_ITEMS: usize = 65_536;
    pub const MAX_DIRECTORY_RENDERED_VIEWPORTS: usize = 8;
}

impl Default for ResourceLimitConfig {
    fn default() -> Self {
        Self {
            directory_page_items: 512,
            directory_prefetch_pages: 2,
            directory_retained_items: 4_096,
            directory_rendered_viewports: 3,
        }
    }
}

/// A validated, immutable set of limits captured when an operation starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    directory_page_items: NonZeroUsize,
    directory_prefetch_pages: NonZeroUsize,
    directory_retained_items: NonZeroUsize,
    directory_rendered_viewports: NonZeroUsize,
}

impl ResourceLimits {
    #[must_use]
    pub fn snapshot(&self) -> Self {
        self.clone()
    }

    #[must_use]
    pub fn directory_page_items(&self) -> usize {
        self.directory_page_items.get()
    }

    #[must_use]
    pub fn directory_prefetch_pages(&self) -> usize {
        self.directory_prefetch_pages.get()
    }

    #[must_use]
    pub fn directory_retained_items(&self) -> usize {
        self.directory_retained_items.get()
    }

    #[must_use]
    pub fn directory_rendered_viewports(&self) -> usize {
        self.directory_rendered_viewports.get()
    }
}

impl TryFrom<ResourceLimitConfig> for ResourceLimits {
    type Error = CoreError;

    fn try_from(config: ResourceLimitConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            directory_page_items: validate_limit(
                "directory_page_items",
                config.directory_page_items,
                ResourceLimitConfig::MAX_DIRECTORY_PAGE_ITEMS,
            )?,
            directory_prefetch_pages: validate_limit(
                "directory_prefetch_pages",
                config.directory_prefetch_pages,
                ResourceLimitConfig::MAX_DIRECTORY_PREFETCH_PAGES,
            )?,
            directory_retained_items: validate_limit(
                "directory_retained_items",
                config.directory_retained_items,
                ResourceLimitConfig::MAX_DIRECTORY_RETAINED_ITEMS,
            )?,
            directory_rendered_viewports: validate_limit(
                "directory_rendered_viewports",
                config.directory_rendered_viewports,
                ResourceLimitConfig::MAX_DIRECTORY_RENDERED_VIEWPORTS,
            )?,
        })
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self::try_from(ResourceLimitConfig::default())
            .expect("built-in resource limits must remain valid")
    }
}

fn validate_limit(
    field: &'static str,
    value: usize,
    maximum: usize,
) -> Result<NonZeroUsize, CoreError> {
    NonZeroUsize::new(value)
        .filter(|value| value.get() <= maximum)
        .ok_or(CoreError::InvalidResourceLimit {
            field,
            value,
            maximum,
        })
}
