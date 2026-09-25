use crate::error::validate_bounded_bytes;
use crate::{CoreError, ResourceLimits, StoreError};

/// An opaque provider-owned cursor for the next page.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Continuation(Box<[u8]>);

impl Continuation {
    pub const MAX_BYTES: usize = 4_096;

    pub fn new(bytes: impl Into<Box<[u8]>>) -> Result<Self, CoreError> {
        validate_bounded_bytes(bytes, Self::MAX_BYTES, CoreError::InvalidContinuation).map(Self)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn from_usize(value: usize) -> Self {
        Self((value as u64).to_be_bytes().into())
    }

    pub fn decode_usize(&self) -> Result<usize, StoreError> {
        let encoded: [u8; size_of::<u64>()] = self
            .as_bytes()
            .try_into()
            .map_err(|_| StoreError::InvalidContinuation)?;
        usize::try_from(u64::from_be_bytes(encoded)).map_err(|_| StoreError::InvalidContinuation)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRequest {
    page_size: usize,
    continuation: Option<Continuation>,
}

impl PageRequest {
    #[must_use]
    pub fn first(limits: &ResourceLimits) -> Self {
        Self {
            page_size: limits.directory_page_items(),
            continuation: None,
        }
    }

    pub fn new(page_size: usize, continuation: Option<Continuation>) -> Result<Self, StoreError> {
        if page_size == 0 || page_size > ResourceLimits::MAX_DIRECTORY_PAGE_ITEMS {
            return Err(StoreError::InvalidLimit {
                resource: "directory page items",
                value: page_size,
                minimum: 1,
                maximum: ResourceLimits::MAX_DIRECTORY_PAGE_ITEMS,
            });
        }
        Ok(Self {
            page_size,
            continuation,
        })
    }

    #[must_use]
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    #[must_use]
    pub fn continuation(&self) -> Option<&Continuation> {
        self.continuation.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TotalHint {
    Exact(u64),
    AtLeast(u64),
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    items: Vec<T>,
    next: Option<Continuation>,
    page_size: usize,
    total_hint: TotalHint,
}

impl<T> Page<T> {
    pub fn try_new(
        request: &PageRequest,
        items: Vec<T>,
        next: Option<Continuation>,
        total_hint: TotalHint,
    ) -> Result<Self, StoreError> {
        if items.len() > request.page_size {
            return Err(StoreError::PageTooLarge {
                requested: request.page_size,
                returned: items.len(),
            });
        }
        Ok(Self {
            items,
            next,
            page_size: request.page_size,
            total_hint,
        })
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    #[must_use]
    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    #[must_use]
    pub fn total_hint(&self) -> TotalHint {
        self.total_hint
    }

    #[must_use]
    pub fn next_request(&self) -> Option<PageRequest> {
        self.next.clone().map(|continuation| PageRequest {
            page_size: self.page_size,
            continuation: Some(continuation),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagingPolicy {
    page_size: usize,
    prefetch_pages: usize,
    max_retained_items: usize,
}

impl From<&ResourceLimits> for PagingPolicy {
    fn from(limits: &ResourceLimits) -> Self {
        Self {
            page_size: limits.directory_page_items(),
            prefetch_pages: limits.directory_prefetch_pages(),
            max_retained_items: limits.directory_retained_items(),
        }
    }
}

impl PagingPolicy {
    #[must_use]
    pub fn page_size(self) -> usize {
        self.page_size
    }

    #[must_use]
    pub fn prefetch_pages(self) -> usize {
        self.prefetch_pages
    }

    #[must_use]
    pub fn max_retained_items(self) -> usize {
        self.max_retained_items
    }
}
