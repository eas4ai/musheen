use crate::search::DirectoryFilter;
use crate::views::{DirectoryViewModel, SelectionMode, ViewPreferences};
use musheen_core::{DisplayPath, ItemId, ItemKind, StoreItem, StorePath};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use tempfile::{Builder, NamedTempFile, TempDir, TempPath};

const MAGIC: &[u8; 8] = b"MSIDX001";
/// Every index directory starts with this prefix; the startup sweep looks
/// only at such entries.
const INDEX_DIRECTORY_PREFIX: &str = "musheen-directory-";
/// The file inside an index directory that its owner keeps locked for as
/// long as the index is in use.
const INDEX_LOCK_NAME: &str = "lock";

/// The directory that holds every tab's index: `$XDG_CACHE_HOME/musheen/directory-index`,
/// or `~/.cache/musheen/directory-index` when `XDG_CACHE_HOME` is unset. Without a
/// usable home the index falls back to the temporary directory.
pub(crate) fn directory_index_root() -> PathBuf {
    directory_index_root_from(
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .unwrap_or_else(|| std::env::temp_dir().join("musheen-directory-index"))
}

/// Resolves the index root from the cache and home variables, following the
/// XDG base directory rule that a relative `XDG_CACHE_HOME` is ignored.
pub(crate) fn directory_index_root_from(
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    let cache = xdg_cache_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })?;
    Some(cache.join("musheen").join("directory-index"))
}

/// Index directories owned by this process, removed by the termination watcher.
fn live_indexes() -> &'static Mutex<BTreeSet<PathBuf>> {
    static LIVE: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Removes every index directory this process still owns. The termination
/// watcher calls it before the process ends by signal; the owners' own
/// cleanup never runs on that path.
pub(crate) fn remove_live_indexes() -> usize {
    let paths = live_indexes()
        .lock()
        .map(|mut live| std::mem::take(&mut *live))
        .unwrap_or_else(|poisoned| std::mem::take(&mut *poisoned.into_inner()));
    paths
        .into_iter()
        .filter(|path| fs::remove_dir_all(path).is_ok())
        .count()
}

/// Removes index directories under `root` whose owner no longer holds the
/// lock: leftovers of a process that ended without cleanup. An index whose
/// lock is held stays; it belongs to a running process.
pub(crate) fn sweep_stale_indexes(root: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut removed = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .as_bytes()
            .starts_with(INDEX_DIRECTORY_PREFIX.as_bytes())
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let path = entry.path();
        let lock = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join(INDEX_LOCK_NAME))
        {
            Ok(file) => match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(lock) => Some(lock),
                // The owner is alive and holds the lock.
                Err((_, nix::errno::Errno::EWOULDBLOCK)) => continue,
                Err((_, errno)) => return Err(io::Error::from(errno)),
            },
            // A directory without its lock file never finished being created.
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        fs::remove_dir_all(&path)?;
        drop(lock);
        removed.push(path);
    }
    Ok(removed)
}

fn lock_index_directory(directory: &Path) -> io::Result<Flock<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(INDEX_LOCK_NAME))?;
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| io::Error::from(errno))
}
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const SORT_RUN_ITEMS: usize = 4_096;
const MERGE_FAN_IN: usize = 32;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexRecord {
    id: ItemId,
    path: StorePath,
    display_name: String,
    kind: u8,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
    arrival: u64,
    #[serde(default)]
    deleted: bool,
}

impl IndexRecord {
    fn from_item(item: &StoreItem, arrival: u64) -> Self {
        let kind = match item.kind() {
            ItemKind::Directory => 0,
            ItemKind::RegularFile => 1,
            ItemKind::SymbolicLink => 2,
            ItemKind::Other => 3,
        };
        Self {
            id: item.id().clone(),
            path: item.path().clone(),
            display_name: item.display_name().as_str().to_owned(),
            kind,
            size: item.size(),
            modified_unix_seconds: item.modified_unix_seconds(),
            arrival,
            deleted: false,
        }
    }

    fn tombstone(id: &ItemId, arrival: u64) -> Self {
        Self {
            id: id.clone(),
            // Tombstones are considered only by ID and arrival. These fields
            // are never returned from the active identity or visible orders.
            path: StorePath::from_unix_path("/"),
            display_name: String::new(),
            kind: 3,
            size: None,
            modified_unix_seconds: None,
            arrival,
            deleted: true,
        }
    }

    fn into_item(self) -> io::Result<(StoreItem, u64)> {
        let kind = match self.kind {
            0 => ItemKind::Directory,
            1 => ItemKind::RegularFile,
            2 => ItemKind::SymbolicLink,
            3 => ItemKind::Other,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid item kind",
                ));
            }
        };
        let mut item = StoreItem::new(
            self.id,
            self.path,
            DisplayPath::new(self.display_name),
            kind,
            self.size,
        );
        if let Some(modified) = self.modified_unix_seconds {
            item = item.with_modified_unix_seconds(modified);
        }
        Ok((item, self.arrival))
    }
}

#[derive(Debug)]
pub(super) struct DiskDirectoryIndex {
    records: File,
    offsets: File,
    record_count: u64,
    order: Option<SortedOrder>,
    order_preferences: Option<ViewPreferences>,
    id_order: Option<SortedOrder>,
    /// Held for the life of the index; `lock` drops before `scratch` so the
    /// directory is removed after the lock is released.
    _lock: Flock<File>,
    scratch: TempDir,
}

impl Drop for DiskDirectoryIndex {
    fn drop(&mut self) {
        live_indexes()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(self.scratch.path());
    }
}

#[derive(Debug)]
struct SortedOrder {
    file: File,
    _path: TempPath,
    len: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedSelection {
    bits: Vec<u64>,
    count: usize,
}

pub(crate) struct ResolvedIndexedSelection {
    pub(crate) targets: Vec<(ItemId, StorePath)>,
    pub(crate) first_item: Option<StoreItem>,
}

impl IndexedSelection {
    fn new(record_count: u64) -> io::Result<Self> {
        let count = usize::try_from(record_count).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory count exceeds selection limits",
            )
        })?;
        Ok(Self {
            bits: vec![0; count.div_ceil(64)],
            count: 0,
        })
    }

    fn insert(&mut self, arrival: u64) -> io::Result<()> {
        let ordinal = usize::try_from(arrival).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "arrival exceeds selection limits",
            )
        })?;
        let Some(word) = self.bits.get_mut(ordinal / 64) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "arrival exceeds index count",
            ));
        };
        let mask = 1u64 << (ordinal % 64);
        if *word & mask == 0 {
            *word |= mask;
            self.count += 1;
        }
        Ok(())
    }

    /// Removes one arrival; returns whether it was selected.
    fn remove(&mut self, arrival: u64) -> bool {
        let Some(ordinal) = usize::try_from(arrival).ok() else {
            return false;
        };
        let Some(word) = self.bits.get_mut(ordinal / 64) else {
            return false;
        };
        let mask = 1u64 << (ordinal % 64);
        if *word & mask == 0 {
            return false;
        }
        *word &= !mask;
        self.count -= 1;
        true
    }

    fn toggle(&mut self, arrival: u64) -> io::Result<()> {
        if self.remove(arrival) {
            Ok(())
        } else {
            self.insert(arrival)
        }
    }

    pub(crate) fn contains(&self, arrival: u64) -> bool {
        usize::try_from(arrival)
            .ok()
            .and_then(|ordinal| {
                self.bits
                    .get(ordinal / 64)
                    .map(|word| word & (1u64 << (ordinal % 64)) != 0)
            })
            .unwrap_or(false)
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn carry_forward(
        &mut self,
        old_arrival: u64,
        new_arrival: Option<u64>,
    ) -> io::Result<()> {
        if !self.contains(old_arrival) {
            return Ok(());
        }
        if let Some(new_arrival) = new_arrival {
            let ordinal = usize::try_from(new_arrival).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "arrival exceeds selection limits",
                )
            })?;
            let required = ordinal
                .checked_div(64)
                .and_then(|word| word.checked_add(1))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "selection size overflow")
                })?;
            if required > self.bits.len() {
                self.bits
                    .try_reserve(required - self.bits.len())
                    .map_err(io::Error::other)?;
                self.bits.resize(required, 0);
            }
        }
        let old = usize::try_from(old_arrival).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "arrival exceeds selection limits",
            )
        })?;
        self.bits[old / 64] &= !(1u64 << (old % 64));
        self.count -= 1;
        if let Some(new_arrival) = new_arrival {
            self.insert(new_arrival)?;
        }
        Ok(())
    }
}

struct SortEntry {
    item: StoreItem,
    arrival: u64,
    offset: u64,
}

#[derive(Clone, Copy)]
enum OrderPolicy<'a> {
    Visible(&'a ViewPreferences),
    Identity,
}

impl DiskDirectoryIndex {
    /// Creates an index directory under `root`, creating `root` when needed.
    /// The directory holds a lock file this process keeps locked, so a later
    /// process can tell a live index from one left behind.
    pub(super) fn new_in(root: &Path) -> io::Result<Self> {
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        let scratch = Builder::new()
            .prefix(INDEX_DIRECTORY_PREFIX)
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(root)?;
        let lock = lock_index_directory(scratch.path())?;
        let mut records = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(scratch.path().join("records"))?;
        records.write_all(MAGIC)?;
        let offsets = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(scratch.path().join("offsets"))?;
        live_indexes()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(scratch.path().to_path_buf());
        Ok(Self {
            records,
            offsets,
            record_count: 0,
            order: None,
            order_preferences: None,
            id_order: None,
            _lock: lock,
            scratch,
        })
    }

    /// The directory that holds this index's files.
    #[cfg(test)]
    pub(super) fn path(&self) -> &Path {
        self.scratch.path()
    }

    pub(super) fn append(&mut self, item: &StoreItem, arrival: u64) -> io::Result<u64> {
        self.append_record(IndexRecord::from_item(item, arrival))
    }

    pub(super) fn append_tombstone(&mut self, id: &ItemId, arrival: u64) -> io::Result<u64> {
        self.append_record(IndexRecord::tombstone(id, arrival))
    }

    fn append_record(&mut self, record: IndexRecord) -> io::Result<u64> {
        let bytes = serde_json::to_vec(&record)?;
        if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory item record exceeds the index limit",
            ));
        }
        let length = u32::try_from(bytes.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory item record is too large",
            )
        })?;
        let next_count = self.record_count.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "directory index count overflow")
        })?;
        let offset = self.records.seek(SeekFrom::End(0))?;
        let offsets_len = self.offsets.seek(SeekFrom::End(0))?;
        let written = (|| {
            self.records.write_all(&length.to_le_bytes())?;
            self.records.write_all(&bytes)?;
            self.offsets.write_all(&offset.to_le_bytes())
        })();
        if let Err(error) = written {
            let rollback = (|| -> io::Result<()> {
                if self.records.metadata()?.len() != offset {
                    self.records.set_len(offset)?;
                }
                if self.offsets.metadata()?.len() != offsets_len {
                    self.offsets.set_len(offsets_len)?;
                }
                Ok(())
            })();
            if let Err(rollback_error) = rollback {
                return Err(io::Error::other(format!(
                    "directory index append failed: {error}; rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
        self.record_count = next_count;
        Ok(offset)
    }

    pub(super) fn read(&mut self, offset: u64) -> io::Result<(StoreItem, u64)> {
        Self::read_record(&mut self.records, offset)
    }

    pub(super) fn record_count(&self) -> u64 {
        self.record_count
    }

    pub(super) fn visible_count(&self) -> Option<usize> {
        self.order.as_ref().map(|order| order.len)
    }

    pub(super) fn active_count(&self) -> Option<usize> {
        self.id_order.as_ref().map(|order| order.len)
    }

    fn read_record(records: &mut File, offset: u64) -> io::Result<(StoreItem, u64)> {
        Self::read_index_record(records, offset)?.into_item()
    }

    fn read_index_record(records: &mut File, offset: u64) -> io::Result<IndexRecord> {
        if offset < MAGIC.len() as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory record offset is invalid",
            ));
        }
        records.seek(SeekFrom::Start(offset))?;
        let mut length = [0; 4];
        records.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory record length is invalid",
            ));
        }
        let mut bytes = vec![0; length];
        records.read_exact(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(Into::into)
    }

    pub(super) fn rebuild_order(
        &mut self,
        preferences: &ViewPreferences,
        filter: Option<&DirectoryFilter>,
    ) -> io::Result<()> {
        let source = self.scratch.path().join("offsets");
        let all_ids = self.build_order(OrderPolicy::Identity, None, &source, self.record_count)?;
        let id_order = self.deduplicate_id_order(all_ids)?;
        let order = self.build_order(
            OrderPolicy::Visible(preferences),
            filter,
            &id_order._path,
            id_order.len as u64,
        )?;
        self.order = Some(order);
        self.order_preferences = Some(preferences.clone());
        self.id_order = Some(id_order);
        Ok(())
    }

    fn deduplicate_id_order(&mut self, mut all_ids: SortedOrder) -> io::Result<SortedOrder> {
        let mut active = NamedTempFile::new_in(self.scratch.path())?;
        let mut previous = None;
        let mut len = 0usize;
        for _ in 0..all_ids.len {
            let offset = read_next_offset(&mut all_ids.file)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "identity offset is missing")
            })?;
            let record = Self::read_index_record(&mut self.records, offset)?;
            let id = record.id;
            if previous.as_ref() == Some(&id) {
                continue;
            }
            if !record.deleted {
                active.write_all(&offset.to_le_bytes())?;
                len += 1;
            }
            previous = Some(id);
        }
        let path = active.into_temp_path();
        Ok(SortedOrder {
            file: File::open(&path)?,
            _path: path,
            len,
        })
    }

    fn build_order(
        &mut self,
        policy: OrderPolicy<'_>,
        filter: Option<&DirectoryFilter>,
        source: &Path,
        source_count: u64,
    ) -> io::Result<SortedOrder> {
        let offsets = File::open(source)?;
        let mut offsets = BufReader::new(offsets);
        let mut chunk = Vec::with_capacity(SORT_RUN_ITEMS);
        let mut levels: Vec<Vec<TempPath>> = Vec::new();
        let mut visible_count = 0usize;
        for _ in 0..source_count {
            let offset = read_next_offset(&mut offsets)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "directory offset is missing")
            })?;
            let (item, arrival) = self.read(offset)?;
            if let OrderPolicy::Visible(preferences) = policy
                && ((!preferences.show_hidden && item.display_name().as_str().starts_with('.'))
                    || filter.is_some_and(|filter| !filter.matches(&item)))
            {
                continue;
            }
            visible_count = visible_count.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "visible item count overflow")
            })?;
            chunk.push(SortEntry {
                item,
                arrival,
                offset,
            });
            if chunk.len() == SORT_RUN_ITEMS {
                let run = write_sorted_run(self.scratch.path(), &mut chunk, policy)?;
                self.push_run(&mut levels, run, policy)?;
            }
        }
        if !chunk.is_empty() {
            let run = write_sorted_run(self.scratch.path(), &mut chunk, policy)?;
            self.push_run(&mut levels, run, policy)?;
        }
        // Every level holds fewer than MERGE_FAN_IN paths. The number of
        // levels is bounded by the u64 record count, not by folder size.
        let mut runs = levels.into_iter().flatten().collect::<Vec<_>>();
        if runs.is_empty() {
            runs.push(NamedTempFile::new_in(self.scratch.path())?.into_temp_path());
        }
        while runs.len() > 1 {
            let mut merged = Vec::with_capacity(runs.len().div_ceil(MERGE_FAN_IN));
            for group in runs.chunks(MERGE_FAN_IN) {
                merged.push(self.merge_runs(group, policy)?);
            }
            runs = merged;
        }
        let path = runs.pop().expect("at least one run exists");
        let file = File::open(&path)?;
        Ok(SortedOrder {
            file,
            _path: path,
            len: visible_count,
        })
    }

    fn push_run(
        &mut self,
        levels: &mut Vec<Vec<TempPath>>,
        mut run: TempPath,
        policy: OrderPolicy<'_>,
    ) -> io::Result<()> {
        let mut level = 0;
        loop {
            if levels.len() == level {
                levels.push(Vec::with_capacity(MERGE_FAN_IN));
            }
            levels[level].push(run);
            if levels[level].len() < MERGE_FAN_IN {
                return Ok(());
            }
            run = self.merge_runs(&levels[level], policy)?;
            levels[level].clear();
            level += 1;
        }
    }

    fn merge_runs(&mut self, runs: &[TempPath], policy: OrderPolicy<'_>) -> io::Result<TempPath> {
        let mut readers = Vec::with_capacity(runs.len());
        for path in runs {
            let mut reader = BufReader::new(File::open(path)?);
            let head = match read_next_offset(&mut reader)? {
                Some(offset) => Some(self.sort_entry(offset)?),
                None => None,
            };
            readers.push((reader, head));
        }
        let mut output = NamedTempFile::new_in(self.scratch.path())?;
        loop {
            let mut winner: Option<usize> = None;
            for (index, (_, head)) in readers.iter().enumerate() {
                let Some(candidate) = head else { continue };
                if winner.is_none_or(|current| {
                    compare_entries(candidate, readers[current].1.as_ref().unwrap(), policy)
                        == Ordering::Less
                }) {
                    winner = Some(index);
                }
            }
            let Some(winner) = winner else { break };
            let (reader, head) = &mut readers[winner];
            let offset = head.as_ref().unwrap().offset;
            output.write_all(&offset.to_le_bytes())?;
            *head = match read_next_offset(reader)? {
                Some(next) => Some(self.sort_entry(next)?),
                None => None,
            };
        }
        Ok(output.into_temp_path())
    }

    fn sort_entry(&mut self, offset: u64) -> io::Result<SortEntry> {
        let (item, arrival) = self.read(offset)?;
        Ok(SortEntry {
            item,
            arrival,
            offset,
        })
    }

    pub(super) fn read_range(&mut self, range: Range<usize>) -> io::Result<Vec<StoreItem>> {
        self.read_range_with_arrivals(range)
            .map(|rows| rows.into_iter().map(|(item, _)| item).collect())
    }

    pub(super) fn read_range_with_arrivals(
        &mut self,
        range: Range<usize>,
    ) -> io::Result<Vec<(StoreItem, u64)>> {
        let order = self.order.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "directory order is not ready")
        })?;
        if range.start > range.end || range.end > order.len || range.len() > SORT_RUN_ITEMS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory range exceeds the visible order",
            ));
        }
        let byte_offset = range
            .start
            .checked_mul(8)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range offset overflow"))?;
        order.file.seek(SeekFrom::Start(byte_offset))?;
        let mut items = Vec::with_capacity(range.len());
        for _ in range {
            let mut offset = [0; 8];
            order.file.read_exact(&mut offset)?;
            items.push(Self::read_record(
                &mut self.records,
                u64::from_le_bytes(offset),
            )?);
        }
        Ok(items)
    }

    pub(super) fn selection_bitmap(&mut self, range: Range<usize>) -> io::Result<IndexedSelection> {
        let order = self.order.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "directory order is not ready")
        })?;
        if range.start > range.end || range.end > order.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "selection range exceeds the visible order",
            ));
        }
        let mut selection = IndexedSelection::new(self.record_count)?;
        let byte_offset = range
            .start
            .checked_mul(8)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range offset overflow"))?;
        order.file.seek(SeekFrom::Start(byte_offset))?;
        for _ in range {
            let offset = read_next_offset(&mut order.file)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "directory order is truncated")
            })?;
            let record = Self::read_index_record(&mut self.records, offset)?;
            selection.insert(record.arrival)?;
        }
        Ok(selection)
    }

    /// Toggles `id` in a selection: the bitmap when one exists, else a bitmap
    /// built from the in-memory `base_ids`. `None` when `id` is not indexed.
    pub(super) fn toggle_selection(
        &mut self,
        base: Option<IndexedSelection>,
        base_ids: &[ItemId],
        id: &ItemId,
    ) -> io::Result<Option<IndexedSelection>> {
        let Some((_, arrival)) = self.lookup_id_with_arrival(id)? else {
            return Ok(None);
        };
        let mut selection = match base {
            Some(selection) => selection,
            None => {
                let mut selection = IndexedSelection::new(self.record_count)?;
                for base_id in base_ids {
                    if let Some((_, base_arrival)) = self.lookup_id_with_arrival(base_id)? {
                        selection.insert(base_arrival)?;
                    }
                }
                selection
            }
        };
        selection.toggle(arrival)?;
        Ok(Some(selection))
    }

    /// The selection a rubber band over `positions` (rows of the visible
    /// order) produces from the selection at the gesture start: replaced,
    /// added to, or toggled, as `mode` says.
    pub(super) fn rubber_band_selection(
        &mut self,
        base: Option<IndexedSelection>,
        base_ids: &[ItemId],
        positions: &[usize],
        mode: SelectionMode,
    ) -> io::Result<IndexedSelection> {
        let covered = self.arrivals_at_positions(positions)?;
        let mut selection = match (mode, base) {
            (SelectionMode::Replace, _) => IndexedSelection::new(self.record_count)?,
            (_, Some(base)) => base,
            (_, None) => {
                let mut selection = IndexedSelection::new(self.record_count)?;
                for base_id in base_ids {
                    if let Some((_, arrival)) = self.lookup_id_with_arrival(base_id)? {
                        selection.insert(arrival)?;
                    }
                }
                selection
            }
        };
        for arrival in covered {
            match mode {
                SelectionMode::Replace | SelectionMode::Add => selection.insert(arrival)?,
                SelectionMode::Toggle => selection.toggle(arrival)?,
            }
        }
        Ok(selection)
    }

    fn arrivals_at_positions(&mut self, positions: &[usize]) -> io::Result<Vec<u64>> {
        let order = self.order.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "directory order is not ready")
        })?;
        let mut arrivals = Vec::with_capacity(positions.len());
        for &position in positions {
            if position >= order.len {
                continue;
            }
            let byte_offset = u64::try_from(position)
                .ok()
                .and_then(|value| value.checked_mul(8))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "range offset overflow")
                })?;
            order.file.seek(SeekFrom::Start(byte_offset))?;
            let offset = read_next_offset(&mut order.file)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "directory order is truncated")
            })?;
            arrivals.push(Self::read_index_record(&mut self.records, offset)?.arrival);
        }
        Ok(arrivals)
    }

    pub(super) fn resolve_bitmap(
        &mut self,
        selection: &IndexedSelection,
    ) -> io::Result<ResolvedIndexedSelection> {
        let order = self.id_order.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "directory identity order is not ready",
            )
        })?;
        order.file.seek(SeekFrom::Start(0))?;
        let mut targets = Vec::new();
        let mut first_item = None;
        for _ in 0..order.len {
            let offset = read_next_offset(&mut order.file)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "directory order is truncated")
            })?;
            let record = Self::read_index_record(&mut self.records, offset)?;
            if !selection.contains(record.arrival) {
                continue;
            }
            if first_item.is_none() {
                first_item = Some(record.clone().into_item()?.0);
            }
            targets.push((record.id, record.path));
        }
        Ok(ResolvedIndexedSelection {
            targets,
            first_item,
        })
    }

    pub(super) fn lookup_id(&mut self, id: &ItemId) -> io::Result<Option<StoreItem>> {
        self.lookup_id_entry(id)
            .map(|entry| entry.map(|entry| entry.item))
    }

    pub(super) fn lookup_id_with_arrival(
        &mut self,
        id: &ItemId,
    ) -> io::Result<Option<(StoreItem, u64)>> {
        self.lookup_id_entry(id)
            .map(|entry| entry.map(|entry| (entry.item, entry.arrival)))
    }

    pub(super) fn position_of_id(&mut self, id: &ItemId) -> io::Result<Option<usize>> {
        let Some(target) = self.lookup_id_entry(id)? else {
            return Ok(None);
        };
        let preferences = self.order_preferences.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "directory order policy is not ready",
            )
        })?;
        let order = self.order.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "directory order is not ready")
        })?;
        let mut low = 0;
        let mut high = order.len;
        while low < high {
            let middle = low + (high - low) / 2;
            let candidate = Self::order_entry_at(order, &mut self.records, middle)?;
            if compare_entries(&candidate, &target, OrderPolicy::Visible(preferences))
                == Ordering::Less
            {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low == order.len {
            return Ok(None);
        }
        let candidate = Self::order_entry_at(order, &mut self.records, low)?;
        Ok((candidate.item.id() == id).then_some(low))
    }

    fn lookup_id_entry(&mut self, id: &ItemId) -> io::Result<Option<SortEntry>> {
        let order = self.id_order.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "directory identity order is not ready",
            )
        })?;
        let mut low = 0;
        let mut high = order.len;
        while low < high {
            let middle = low + (high - low) / 2;
            let candidate = Self::order_entry_at(order, &mut self.records, middle)?;
            match candidate.item.id().cmp(id) {
                Ordering::Less => low = middle + 1,
                Ordering::Equal | Ordering::Greater => high = middle,
            }
        }
        if low == order.len {
            return Ok(None);
        }
        let candidate = Self::order_entry_at(order, &mut self.records, low)?;
        Ok((candidate.item.id() == id).then_some(candidate))
    }

    fn order_entry_at(
        order: &mut SortedOrder,
        records: &mut File,
        position: usize,
    ) -> io::Result<SortEntry> {
        let byte_offset = u64::try_from(position)
            .ok()
            .and_then(|value| value.checked_mul(8))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "directory offset overflow")
            })?;
        order.file.seek(SeekFrom::Start(byte_offset))?;
        let offset = read_next_offset(&mut order.file)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "directory order offset is missing",
            )
        })?;
        let (item, arrival) = Self::read_record(records, offset)?;
        Ok(SortEntry {
            item,
            arrival,
            offset,
        })
    }
}

fn compare_entries(left: &SortEntry, right: &SortEntry, policy: OrderPolicy<'_>) -> Ordering {
    match policy {
        OrderPolicy::Visible(preferences) => {
            DirectoryViewModel::compare_with_preferences(preferences, &left.item, &right.item)
                .then_with(|| left.arrival.cmp(&right.arrival))
        }
        OrderPolicy::Identity => left
            .item
            .id()
            .cmp(right.item.id())
            .then_with(|| right.arrival.cmp(&left.arrival)),
    }
}

fn write_sorted_run(
    scratch: &Path,
    chunk: &mut Vec<SortEntry>,
    policy: OrderPolicy<'_>,
) -> io::Result<TempPath> {
    chunk.sort_by(|left, right| compare_entries(left, right, policy));
    let mut output = NamedTempFile::new_in(scratch)?;
    for entry in chunk.drain(..) {
        output.write_all(&entry.offset.to_le_bytes())?;
    }
    Ok(output.into_temp_path())
}

fn read_next_offset(reader: &mut impl Read) -> io::Result<Option<u64>> {
    let mut bytes = [0; 8];
    if reader.read(&mut bytes[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut bytes[1..])?;
    Ok(Some(u64::from_le_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use super::{
        DiskDirectoryIndex, INDEX_DIRECTORY_PREFIX, INDEX_LOCK_NAME, directory_index_root_from,
        lock_index_directory, sweep_stale_indexes,
    };
    use crate::search::DirectoryFilter;
    use crate::views::{SortDirection, SortKey, ViewPreferences};
    use musheen_core::{DisplayPath, ItemId, ItemKind, ProviderId, StoreItem, StorePath};
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::PermissionsExt;

    fn item(path: StorePath) -> StoreItem {
        StoreItem::new(
            ItemId::new(ProviderId::new("local").unwrap(), b"identity".to_vec()).unwrap(),
            path,
            DisplayPath::new("name"),
            ItemKind::SymbolicLink,
            None,
        )
        .with_modified_unix_seconds(17)
    }

    #[test]
    fn record_round_trip_preserves_non_utf8_path() {
        let original = item(StorePath::from_unix_bytes(b"/tmp/name-\xff".to_vec()));
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();

        let offset = index.append(&original, 7).unwrap();
        let (decoded, arrival) = index.read(offset).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(arrival, 7);
        assert_eq!(decoded.path().unix_bytes(), Some(&b"/tmp/name-\xff"[..]));
    }

    #[test]
    fn record_round_trip_preserves_provider_key() {
        let provider = ProviderId::new("remote").unwrap();
        let path = StorePath::from_provider_key(provider, b"opaque-\xff".to_vec()).unwrap();
        let original = item(path);
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();

        let offset = index.append(&original, 19).unwrap();
        let (decoded, arrival) = index.read(offset).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(arrival, 19);
    }

    #[test]
    fn truncated_record_is_rejected() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let offset = index
            .append(&item(StorePath::from_unix_path("/tmp/item")), 1)
            .unwrap();
        index.records.set_len(offset + 2).unwrap();

        assert!(index.read(offset).is_err());
    }

    #[test]
    fn failed_offset_write_rolls_back_record_append() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let original = item(StorePath::from_unix_path("/tmp/item"));
        index.append(&original, 0).unwrap();
        let records_len = index.records.metadata().unwrap().len();
        let offsets_len = index.offsets.metadata().unwrap().len();
        let read_only_offsets = std::fs::File::open(index.scratch.path().join("offsets")).unwrap();
        let writable_offsets = std::mem::replace(&mut index.offsets, read_only_offsets);

        assert!(index.append(&original, 1).is_err());
        assert_eq!(index.record_count(), 1);
        assert_eq!(index.records.metadata().unwrap().len(), records_len);
        assert_eq!(index.offsets.metadata().unwrap().len(), offsets_len);

        index.offsets = writable_offsets;
        let offset = index.append(&original, 1).unwrap();
        assert_eq!(offset, records_len);
        assert_eq!(index.record_count(), 2);
    }

    #[test]
    fn failed_record_write_keeps_existing_index() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let original = item(StorePath::from_unix_path("/tmp/item"));
        index.append(&original, 0).unwrap();
        let records_len = index.records.metadata().unwrap().len();
        let offsets_len = index.offsets.metadata().unwrap().len();
        let read_only_records = std::fs::File::open(index.scratch.path().join("records")).unwrap();
        let writable_records = std::mem::replace(&mut index.records, read_only_records);

        let error = index.append(&original, 1).unwrap_err();
        assert!(!error.to_string().contains("rollback failed"));
        assert_eq!(index.record_count(), 1);
        assert_eq!(index.records.metadata().unwrap().len(), records_len);
        assert_eq!(index.offsets.metadata().unwrap().len(), offsets_len);

        index.records = writable_records;
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        assert_eq!(index.read_range(0..1).unwrap(), vec![original]);
    }

    #[test]
    fn full_device_append_preserves_last_published_order() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let original = item(StorePath::from_unix_path("/tmp/item"));
        index.append(&original, 0).unwrap();
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        let records_len = index.records.metadata().unwrap().len();
        let full_device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/full")
            .unwrap();
        let writable_offsets = std::mem::replace(&mut index.offsets, full_device);

        let error = index.append(&original, 1).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(28));
        assert_eq!(index.record_count(), 1);
        assert_eq!(index.records.metadata().unwrap().len(), records_len);
        assert_eq!(index.visible_count(), Some(1));
        assert_eq!(index.read_range(0..1).unwrap(), vec![original]);

        index.offsets = writable_offsets;
    }

    #[test]
    fn corrupt_new_record_cannot_replace_last_valid_order() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let original = item(StorePath::from_unix_path("/tmp/original"));
        index.append(&original, 0).unwrap();
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        let new_record = item(StorePath::from_unix_path("/tmp/new"));
        let offset = index.append(&new_record, 1).unwrap();
        index.records.seek(SeekFrom::Start(offset)).unwrap();
        index.records.write_all(&0u32.to_le_bytes()).unwrap();

        assert!(
            index
                .rebuild_order(&ViewPreferences::default(), None)
                .is_err()
        );
        assert_eq!(index.visible_count(), Some(1));
        assert_eq!(index.active_count(), Some(1));
        assert_eq!(index.read_range(0..1).unwrap(), vec![original]);
    }

    #[test]
    fn private_record_directory_is_removed_on_drop() {
        let index_root = tempfile::tempdir().unwrap();
        let index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let path = index.scratch.path().to_path_buf();
        assert!(path.is_dir());
        for entry in [&path, &path.join("records"), &path.join("offsets")] {
            let mode = entry.metadata().unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "index scratch must be owner-only: {entry:?}"
            );
        }

        drop(index);

        assert!(!path.exists());
    }

    #[test]
    fn external_order_supports_random_ranges() {
        let provider = ProviderId::new("local").unwrap();
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        for number in (0u64..8_200).rev() {
            let name = format!("item-{number}");
            let item = StoreItem::new(
                ItemId::new(provider.clone(), number.to_be_bytes().to_vec()).unwrap(),
                StorePath::from_unix_path(format!("/many/{name}")),
                DisplayPath::new(name),
                ItemKind::RegularFile,
                Some(number),
            );
            index.append(&item, 8_199 - number).unwrap();
        }

        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        let mut names = |range| {
            index
                .read_range(range)
                .unwrap()
                .iter()
                .map(|item| item.display_name().as_str().to_owned())
                .collect::<Vec<_>>()
        };

        assert_eq!(names(0..3), ["item-0", "item-1", "item-2"]);
        assert_eq!(names(4_095..4_098), ["item-4095", "item-4096", "item-4097"]);
        assert_eq!(names(8_197..8_200), ["item-8197", "item-8198", "item-8199"]);
        assert_eq!(names(0..1), ["item-0"]);
        for number in [0u64, 4_096, 8_199] {
            let id = ItemId::new(provider.clone(), number.to_be_bytes().to_vec()).unwrap();
            assert_eq!(index.position_of_id(&id).unwrap(), Some(number as usize));
        }
    }

    #[test]
    fn identity_lookup_reaches_offscreen_records() {
        let provider = ProviderId::new("local").unwrap();
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        for number in 0u64..8_200 {
            let item = StoreItem::new(
                ItemId::new(provider.clone(), number.to_be_bytes().to_vec()).unwrap(),
                StorePath::from_unix_path(format!("/many/item-{number}")),
                DisplayPath::new(format!("item-{number}")),
                ItemKind::RegularFile,
                Some(number),
            );
            index.append(&item, number).unwrap();
        }
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();

        for number in [0u64, 4_096, 8_199] {
            let id = ItemId::new(provider.clone(), number.to_be_bytes().to_vec()).unwrap();
            let found = index.lookup_id(&id).unwrap().unwrap();
            assert_eq!(found.id(), &id);
            assert_eq!(
                found.path().as_unix_path().unwrap(),
                std::path::Path::new(&format!("/many/item-{number}"))
            );
        }
        let missing = ItemId::new(provider, 8_200u64.to_be_bytes().to_vec()).unwrap();
        assert!(index.lookup_id(&missing).unwrap().is_none());
    }

    #[test]
    fn visible_range_selection_uses_arrival_bits_and_excludes_new_arrivals() {
        let provider = ProviderId::new("local").unwrap();
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        for (number, name) in [(0u64, "c"), (1, ".hidden"), (2, "a"), (3, "b")] {
            let entry = StoreItem::new(
                ItemId::new(provider.clone(), number.to_be_bytes()).unwrap(),
                StorePath::from_unix_path(format!("/many/{name}")),
                DisplayPath::new(name),
                ItemKind::RegularFile,
                None,
            );
            index.append(&entry, number).unwrap();
        }
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();

        let selection = index.selection_bitmap(0..3).unwrap();
        assert_eq!(selection.count(), 3);
        assert!(selection.contains(0));
        assert!(!selection.contains(1));
        assert!(selection.contains(2));
        assert!(selection.contains(3));
        let resolved = index.resolve_bitmap(&selection).unwrap();
        assert_eq!(resolved.targets.len(), 3);
        assert!(
            resolved
                .targets
                .iter()
                .any(|(_, path)| path == &StorePath::from_unix_path("/many/a"))
        );
        assert_eq!(resolved.first_item.unwrap().path(), &resolved.targets[0].1);

        let newcomer = StoreItem::new(
            ItemId::new(provider, 4u64.to_be_bytes()).unwrap(),
            StorePath::from_unix_path("/many/d"),
            DisplayPath::new("d"),
            ItemKind::RegularFile,
            None,
        );
        index.append(&newcomer, 4).unwrap();
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        assert!(!selection.contains(4));
        assert_eq!(index.resolve_bitmap(&selection).unwrap().targets.len(), 3);
        index
            .rebuild_order(
                &ViewPreferences::default(),
                Some(&DirectoryFilter::new("a")),
            )
            .unwrap();
        assert_eq!(index.visible_count(), Some(1));
        assert_eq!(index.resolve_bitmap(&selection).unwrap().targets.len(), 3);
    }

    #[test]
    fn later_record_replaces_same_identity_in_visible_order_and_lookup() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let old = item(StorePath::from_unix_bytes(b"/many/old-\xff".to_vec()));
        let replacement = item(StorePath::from_unix_bytes(b"/many/new-\xff".to_vec()));
        index.append(&old, 0).unwrap();
        index.append(&replacement, 1).unwrap();
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();

        assert_eq!(index.visible_count(), Some(1));
        assert_eq!(
            index.read_range(0..1).unwrap(),
            std::slice::from_ref(&replacement)
        );
        assert_eq!(index.lookup_id(old.id()).unwrap(), Some(replacement));
    }

    #[test]
    fn removal_tombstone_hides_identity_until_a_later_create() {
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let original = item(StorePath::from_unix_path("/many/original"));
        let recreated = item(StorePath::from_unix_path("/many/recreated"));
        index.append(&original, 0).unwrap();
        index.append_tombstone(original.id(), 1).unwrap();
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        assert_eq!(index.visible_count(), Some(0));
        assert_eq!(index.lookup_id(original.id()).unwrap(), None);

        index.append(&recreated, 2).unwrap();
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        assert_eq!(index.visible_count(), Some(1));
        assert_eq!(index.lookup_id(original.id()).unwrap(), Some(recreated));
    }

    #[test]
    fn external_order_matches_view_sort_hidden_and_filter_rules() {
        let provider = ProviderId::new("local").unwrap();
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        for (number, name, kind, size) in [
            (0, "item-10", ItemKind::RegularFile, Some(10)),
            (1, "item-2", ItemKind::RegularFile, Some(2)),
            (2, ".hidden", ItemKind::RegularFile, Some(100)),
            (3, "folder", ItemKind::Directory, None),
            (4, "same", ItemKind::RegularFile, Some(7)),
            (5, "same", ItemKind::RegularFile, Some(7)),
        ] {
            let item = StoreItem::new(
                ItemId::new(provider.clone(), vec![number]).unwrap(),
                StorePath::from_unix_path(format!("/many/{name}-{number}")),
                DisplayPath::new(name),
                kind,
                size,
            );
            index.append(&item, u64::from(number)).unwrap();
        }

        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();
        let names = index
            .read_range(0..5)
            .unwrap()
            .iter()
            .map(|item| item.display_name().as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["folder", "item-2", "item-10", "same", "same"]);
        let duplicate_ids = index
            .read_range(3..5)
            .unwrap()
            .iter()
            .map(|item| item.id().clone())
            .collect::<Vec<_>>();
        assert_eq!(
            duplicate_ids[0],
            ItemId::new(provider.clone(), vec![4]).unwrap()
        );
        assert_eq!(
            duplicate_ids[1],
            ItemId::new(provider.clone(), vec![5]).unwrap()
        );
        assert_eq!(
            index
                .position_of_id(&ItemId::new(provider.clone(), vec![2]).unwrap())
                .unwrap(),
            None
        );
        assert_eq!(
            index
                .position_of_id(&ItemId::new(provider.clone(), vec![1]).unwrap())
                .unwrap(),
            Some(1)
        );

        let mut preferences = ViewPreferences {
            show_hidden: true,
            ..ViewPreferences::default()
        };
        preferences.sort.key = SortKey::Size;
        preferences.sort.direction = SortDirection::Descending;
        index.rebuild_order(&preferences, None).unwrap();
        let names = index
            .read_range(0..6)
            .unwrap()
            .iter()
            .map(|item| item.display_name().as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["folder", ".hidden", "item-10", "same", "same", "item-2"]
        );
        assert_eq!(
            index
                .position_of_id(&ItemId::new(provider.clone(), vec![2]).unwrap())
                .unwrap(),
            Some(1)
        );

        index
            .rebuild_order(&preferences, Some(&DirectoryFilter::new("item")))
            .unwrap();
        let names = index
            .read_range(0..2)
            .unwrap()
            .iter()
            .map(|item| item.display_name().as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["item-10", "item-2"]);
        assert_eq!(
            index
                .position_of_id(&ItemId::new(provider.clone(), vec![2]).unwrap())
                .unwrap(),
            None
        );
        assert_eq!(
            index
                .position_of_id(&ItemId::new(provider, vec![1]).unwrap())
                .unwrap(),
            Some(1)
        );
    }

    #[test]
    #[ignore = "resource-intensive million-item index verification"]
    fn external_order_supports_one_million_records() {
        let provider = ProviderId::new("local").unwrap();
        let index_root = tempfile::tempdir().unwrap();
        let mut index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        for number in (0u64..1_000_000).rev() {
            let name = format!("item-{number}");
            let item = StoreItem::new(
                ItemId::new(provider.clone(), number.to_be_bytes().to_vec()).unwrap(),
                StorePath::from_unix_path(format!("/many/{name}")),
                DisplayPath::new(name),
                ItemKind::RegularFile,
                Some(number),
            );
            index.append(&item, 999_999 - number).unwrap();
        }
        index
            .rebuild_order(&ViewPreferences::default(), None)
            .unwrap();

        assert_eq!(index.record_count, 1_000_000);
        assert_eq!(index.order.as_ref().unwrap().len, 1_000_000);
        for (range, expected) in [
            (0..1, "item-0"),
            (499_999..500_000, "item-499999"),
            (999_999..1_000_000, "item-999999"),
            (0..1, "item-0"),
        ] {
            assert_eq!(
                index.read_range(range).unwrap()[0].display_name().as_str(),
                expected
            );
        }
    }

    #[test]
    fn indexed_folder_root_prefers_xdg_cache_home() {
        let root = directory_index_root_from(
            Some(std::ffi::OsStr::new("/var/cache/me")),
            Some(std::ffi::OsStr::new("/home/me")),
        );
        assert_eq!(
            root.as_deref(),
            Some(std::path::Path::new(
                "/var/cache/me/musheen/directory-index"
            ))
        );
    }

    #[test]
    fn indexed_folder_root_ignores_a_relative_or_empty_xdg_cache_home() {
        for cache in ["relative/cache", ""] {
            let root = directory_index_root_from(
                Some(std::ffi::OsStr::new(cache)),
                Some(std::ffi::OsStr::new("/home/me")),
            );
            assert_eq!(
                root.as_deref(),
                Some(std::path::Path::new(
                    "/home/me/.cache/musheen/directory-index"
                )),
                "XDG_CACHE_HOME={cache:?} falls back to the home cache"
            );
        }
        assert_eq!(directory_index_root_from(None, None), None);
    }

    #[test]
    fn indexed_folder_index_lives_under_its_root_and_holds_the_lock() {
        let index_root = tempfile::tempdir().unwrap();
        let index = DiskDirectoryIndex::new_in(index_root.path()).unwrap();

        let path = index.path().to_path_buf();
        assert_eq!(path.parent(), Some(index_root.path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(INDEX_DIRECTORY_PREFIX)
        );
        assert!(
            lock_index_directory(&path).is_err(),
            "a second lock on a live index is refused"
        );
        assert_eq!(
            sweep_stale_indexes(index_root.path()).unwrap(),
            Vec::<std::path::PathBuf>::new()
        );
        assert!(path.is_dir(), "the sweep keeps a live index");

        drop(index);
        assert!(!path.exists(), "dropping the index removes its directory");
    }

    #[test]
    fn indexed_folder_stale_indexes_are_removed_at_startup() {
        let index_root = tempfile::tempdir().unwrap();
        let stale = index_root.path().join("musheen-directory-stale");
        std::fs::create_dir(&stale).unwrap();
        std::fs::write(stale.join(INDEX_LOCK_NAME), b"").unwrap();
        std::fs::write(stale.join("records"), b"leftover").unwrap();
        let unfinished = index_root.path().join("musheen-directory-unfinished");
        std::fs::create_dir(&unfinished).unwrap();
        let live = DiskDirectoryIndex::new_in(index_root.path()).unwrap();
        let unrelated = index_root.path().join("other-cache-entry");
        std::fs::create_dir(&unrelated).unwrap();

        let mut removed = sweep_stale_indexes(index_root.path()).unwrap();
        removed.sort();

        assert_eq!(removed, vec![stale.clone(), unfinished.clone()]);
        assert!(!stale.exists());
        assert!(!unfinished.exists());
        assert!(live.path().is_dir(), "the running process keeps its index");
        assert!(
            unrelated.is_dir(),
            "entries without the index prefix are left alone"
        );
        assert!(
            sweep_stale_indexes(&index_root.path().join("missing"))
                .unwrap()
                .is_empty(),
            "a missing root has nothing to sweep"
        );
    }
}
