use crate::metadata::{io_error, item_from_path};
use musheen_core::{ProviderId, StoreError, StoreItem, StorePath};
use walkdir::{IntoIter, WalkDir};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraversalOptions {
    pub follow_symlinks: bool,
    pub max_open_descriptors: usize,
    pub max_depth: usize,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            max_open_descriptors: 16,
            max_depth: usize::MAX,
        }
    }
}

pub struct LocalTraversal {
    provider: ProviderId,
    entries: IntoIter,
}

impl LocalTraversal {
    pub(crate) fn new(
        provider: ProviderId,
        root: &StorePath,
        options: TraversalOptions,
    ) -> Result<Self, StoreError> {
        let path = root.as_unix_path().ok_or_else(|| {
            StoreError::unsupported("traverse", "the local provider accepts only Unix paths")
        })?;
        if options.max_open_descriptors == 0 || options.max_open_descriptors > 64 {
            return Err(StoreError::InvalidLimit {
                resource: "traversal open descriptors",
                value: options.max_open_descriptors,
                minimum: 1,
                maximum: 64,
            });
        }
        let entries = WalkDir::new(path)
            .follow_links(options.follow_symlinks)
            .max_open(options.max_open_descriptors)
            .max_depth(options.max_depth)
            .into_iter();
        Ok(Self { provider, entries })
    }
}

impl Iterator for LocalTraversal {
    type Item = Result<StoreItem, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next().map(|entry| match entry {
            Ok(entry) => item_from_path(&self.provider, entry.path()),
            Err(error) => {
                let path = error
                    .path()
                    .map(|path| StorePath::from_unix_path(path.as_os_str().to_os_string()));
                let mapped = error
                    .io_error()
                    .map(|source| {
                        io_error(
                            "traverse directory",
                            path,
                            std::io::Error::from(source.kind()),
                        )
                    })
                    .unwrap_or_else(|| StoreError::Backend(error.to_string().into()));
                Err(mapped)
            }
        })
    }
}
