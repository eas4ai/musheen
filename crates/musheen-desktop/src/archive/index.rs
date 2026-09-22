use super::format::{ArchiveScanner, RawArchiveEntry, RawEntryKind};
use super::path::ArchivePath;
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, DecodeCounterState, elapsed_limit,
};
use musheen_core::{CancellationToken, ItemKind};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

pub(crate) type ArchiveScannerFactory =
    Arc<dyn Fn() -> Result<Box<dyn ArchiveScanner>, ArchiveError> + Send + Sync>;

struct ResettingScanner;

impl ArchiveScanner for ResettingScanner {
    fn next_entry(
        &mut self,
        _cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        Err(ArchiveError::InvalidArchive)
    }
}

pub(crate) struct LazyArchiveIndex {
    scanner: Box<dyn ArchiveScanner>,
    scanner_factory: ArchiveScannerFactory,
    pub(crate) entries: BTreeMap<ArchivePath, IndexedEntry>,
    pub(crate) children: BTreeMap<ArchivePath, Vec<ArchivePath>>,
    pub(crate) finished: bool,
    failure: Option<ArchiveError>,
    source_bytes: u64,
    accepted_entries: usize,
}

pub(crate) struct IndexedEntry {
    pub(crate) kind: ItemKind,
    pub(crate) size: Option<u64>,
    pub(crate) compressed_size: Option<u64>,
    pub(crate) locator: Option<u64>,
    explicit: bool,
    _allocation: AllocationLease,
}

impl LazyArchiveIndex {
    pub(crate) fn new(
        scanner: Box<dyn ArchiveScanner>,
        scanner_factory: ArchiveScannerFactory,
        source_bytes: u64,
    ) -> Self {
        Self {
            scanner,
            scanner_factory,
            entries: BTreeMap::new(),
            children: BTreeMap::new(),
            finished: false,
            failure: None,
            source_bytes,
            accepted_entries: 0,
        }
    }

    pub(crate) fn ensure_directory(
        &mut self,
        location: &ArchivePath,
        cancellation: &CancellationToken,
        limits: &ArchiveLimits,
        counters: &Arc<DecodeCounterState>,
    ) -> Result<(), ArchiveError> {
        self.check_failure()?;
        if location.is_root() {
            return Ok(());
        }
        while !self.finished && !self.entries.contains_key(location) {
            self.scan_one(cancellation, limits, counters)?;
        }
        match self.entries.get(location) {
            Some(entry) if entry.kind == ItemKind::Directory => Ok(()),
            Some(_) | None => Err(ArchiveError::NotArchiveEntry),
        }
    }

    pub(crate) fn ensure_children(
        &mut self,
        location: &ArchivePath,
        required: usize,
        cancellation: &CancellationToken,
        limits: &ArchiveLimits,
        counters: &Arc<DecodeCounterState>,
    ) -> Result<(), ArchiveError> {
        self.check_failure()?;
        while !self.finished && self.children.get(location).map_or(0, Vec::len) < required {
            self.scan_one(cancellation, limits, counters)?;
        }
        Ok(())
    }

    pub(crate) fn find_entry(
        &mut self,
        path: &ArchivePath,
        cancellation: &CancellationToken,
        limits: &ArchiveLimits,
        counters: &Arc<DecodeCounterState>,
    ) -> Result<Option<&IndexedEntry>, ArchiveError> {
        self.check_failure()?;
        while !self.finished && !self.entries.contains_key(path) {
            self.scan_one(cancellation, limits, counters)?;
        }
        Ok(self.entries.get(path))
    }

    fn scan_one(
        &mut self,
        cancellation: &CancellationToken,
        limits: &ArchiveLimits,
        counters: &Arc<DecodeCounterState>,
    ) -> Result<(), ArchiveError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
        if counters.elapsed() > limits.max_elapsed {
            return Err(elapsed_limit(counters.elapsed(), limits.max_elapsed));
        }
        let started = Instant::now();
        let raw = self.scanner.next_entry(cancellation);
        counters.add_elapsed(started.elapsed());
        if counters.elapsed() > limits.max_elapsed {
            return self.fail(elapsed_limit(counters.elapsed(), limits.max_elapsed));
        }
        let raw = match raw {
            Ok(raw) => raw,
            Err(ArchiveError::Cancelled) => {
                if let Err(error) = self.rebuild_scanner() {
                    return self.fail(error);
                }
                return Err(ArchiveError::Cancelled);
            }
            Err(error) => return self.fail(error),
        };
        let Some(raw) = raw else {
            self.finished = true;
            return Ok(());
        };
        if let Some(size) = raw.size
            && let Err(error) = counters.charge_expanded(size, self.source_bytes, limits)
        {
            return self.fail(error);
        }
        let started = Instant::now();
        let result = self.ingest(raw, limits, counters);
        counters.add_elapsed(started.elapsed());
        if counters.elapsed() > limits.max_elapsed {
            return self.fail(elapsed_limit(counters.elapsed(), limits.max_elapsed));
        }
        match result {
            Ok(()) => {
                self.accepted_entries = self.accepted_entries.saturating_add(1);
                Ok(())
            }
            Err(error) => self.fail(error),
        }
    }

    fn rebuild_scanner(&mut self) -> Result<(), ArchiveError> {
        // Release codec-retained metadata before constructing the replacement. Otherwise a retry
        // can transiently require twice the configured budget for two copies of the same index.
        drop(std::mem::replace(
            &mut self.scanner,
            Box::new(ResettingScanner),
        ));
        let mut scanner = (self.scanner_factory)()?;
        let replay = CancellationToken::new();
        for _ in 0..self.accepted_entries {
            scanner
                .next_entry(&replay)?
                .ok_or(ArchiveError::InvalidArchive)?;
        }
        self.scanner = scanner;
        Ok(())
    }

    fn fail<T>(&mut self, error: ArchiveError) -> Result<T, ArchiveError> {
        self.finished = true;
        self.failure = Some(error.clone());
        Err(error)
    }

    fn check_failure(&self) -> Result<(), ArchiveError> {
        self.failure.clone().map_or(Ok(()), Err)
    }

    fn ingest(
        &mut self,
        raw: RawArchiveEntry,
        limits: &ArchiveLimits,
        counters: &Arc<DecodeCounterState>,
    ) -> Result<(), ArchiveError> {
        let path = ArchivePath::from_normalized(raw.provider.clone(), raw.path.clone());
        if path.is_root() {
            return Err(ArchiveError::UnsafePath(
                "an archive entry cannot resolve to the archive root",
            ));
        }
        if self.entries.get(&path).is_some_and(|entry| entry.explicit) {
            return Err(ArchiveError::DuplicatePath);
        }
        for ancestor in path.ancestors() {
            if self
                .entries
                .get(&ancestor)
                .is_some_and(|entry| entry.kind != ItemKind::Directory)
            {
                return Err(ArchiveError::DuplicatePath);
            }
            if !self.entries.contains_key(&ancestor) {
                self.insert_entry(
                    ancestor,
                    ItemKind::Directory,
                    None,
                    None,
                    None,
                    false,
                    limits,
                    counters,
                )?;
            }
        }
        let kind = match raw.kind {
            RawEntryKind::Directory => ItemKind::Directory,
            RawEntryKind::RegularFile => ItemKind::RegularFile,
            RawEntryKind::SymbolicLink => ItemKind::SymbolicLink,
            RawEntryKind::HardLink | RawEntryKind::Other => ItemKind::Other,
        };
        if let Some(existing) = self.entries.get_mut(&path) {
            if existing.kind != ItemKind::Directory || kind != ItemKind::Directory {
                return Err(ArchiveError::DuplicatePath);
            }
            existing.explicit = true;
            existing.size = raw.size;
            existing.compressed_size = raw.compressed_size;
            existing.locator = Some(raw.ordinal);
        } else {
            self.insert_entry(
                path,
                kind,
                raw.size,
                raw.compressed_size,
                Some(raw.ordinal),
                true,
                limits,
                counters,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_entry(
        &mut self,
        path: ArchivePath,
        kind: ItemKind,
        size: Option<u64>,
        compressed_size: Option<u64>,
        locator: Option<u64>,
        explicit: bool,
        limits: &ArchiveLimits,
        counters: &Arc<DecodeCounterState>,
    ) -> Result<(), ArchiveError> {
        if self.entries.len() >= limits.max_entries {
            return Err(ArchiveError::LimitExceeded {
                resource: "archive entries",
                value: self.entries.len() + 1,
                maximum: limits.max_entries,
            });
        }
        let allocation_bytes = path
            .as_bytes()
            .len()
            .saturating_mul(2)
            .saturating_add(std::mem::size_of::<ArchivePath>() * 2)
            .saturating_add(std::mem::size_of::<IndexedEntry>())
            .saturating_add(3 * std::mem::size_of::<usize>());
        let allocation = counters.reserve(allocation_bytes, limits.max_metadata_bytes)?;
        let parent = path.parent().expect("indexed entries are not roots");
        self.children.entry(parent).or_default().push(path.clone());
        self.entries.insert(
            path,
            IndexedEntry {
                kind,
                size,
                compressed_size,
                locator,
                explicit,
                _allocation: allocation,
            },
        );
        Ok(())
    }
}
