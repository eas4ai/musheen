use crate::search::DirectoryFilter;
use crate::views::{DirectoryViewModel, ViewPreferences};
use musheen_core::{DisplayPath, ItemId, ItemKind, StoreItem, StorePath};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use tempfile::{Builder, NamedTempFile, TempDir, TempPath};

const MAGIC: &[u8; 8] = b"MSIDX001";
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const SORT_RUN_ITEMS: usize = 4_096;
const MERGE_FAN_IN: usize = 32;

#[derive(Serialize, Deserialize)]
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
    id_order: Option<SortedOrder>,
    scratch: TempDir,
}

#[derive(Debug)]
struct SortedOrder {
    file: File,
    _path: TempPath,
    len: usize,
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
    pub(super) fn new() -> io::Result<Self> {
        let scratch = Builder::new().prefix("musheen-directory-").tempdir()?;
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
        Ok(Self {
            records,
            offsets,
            record_count: 0,
            order: None,
            id_order: None,
            scratch,
        })
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
        let offset = self.records.seek(SeekFrom::End(0))?;
        self.records.write_all(&length.to_le_bytes())?;
        self.records.write_all(&bytes)?;
        self.offsets.seek(SeekFrom::End(0))?;
        self.offsets.write_all(&offset.to_le_bytes())?;
        self.record_count = self.record_count.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "directory index count overflow")
        })?;
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
            items.push(Self::read_record(&mut self.records, u64::from_le_bytes(offset))?.0);
        }
        Ok(items)
    }

    pub(super) fn lookup_id(&mut self, id: &ItemId) -> io::Result<Option<StoreItem>> {
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
            let position = u64::try_from(middle)
                .ok()
                .and_then(|value| value.checked_mul(8))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "identity offset overflow")
                })?;
            order.file.seek(SeekFrom::Start(position))?;
            let offset = read_next_offset(&mut order.file)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "directory identity offset is missing",
                )
            })?;
            let item = Self::read_record(&mut self.records, offset)?.0;
            match item.id().cmp(id) {
                Ordering::Less => low = middle + 1,
                Ordering::Equal | Ordering::Greater => high = middle,
            }
        }
        if low == order.len {
            return Ok(None);
        }
        let position = u64::try_from(low)
            .ok()
            .and_then(|value| value.checked_mul(8))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "identity offset overflow")
            })?;
        order.file.seek(SeekFrom::Start(position))?;
        let offset = read_next_offset(&mut order.file)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "directory identity offset is missing",
            )
        })?;
        let item = Self::read_record(&mut self.records, offset)?.0;
        Ok((item.id() == id).then_some(item))
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
    use super::DiskDirectoryIndex;
    use crate::search::DirectoryFilter;
    use crate::views::{SortDirection, SortKey, ViewPreferences};
    use musheen_core::{DisplayPath, ItemId, ItemKind, ProviderId, StoreItem, StorePath};

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
        let mut index = DiskDirectoryIndex::new().unwrap();

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
        let mut index = DiskDirectoryIndex::new().unwrap();

        let offset = index.append(&original, 19).unwrap();
        let (decoded, arrival) = index.read(offset).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(arrival, 19);
    }

    #[test]
    fn truncated_record_is_rejected() {
        let mut index = DiskDirectoryIndex::new().unwrap();
        let offset = index
            .append(&item(StorePath::from_unix_path("/tmp/item")), 1)
            .unwrap();
        index.records.set_len(offset + 2).unwrap();

        assert!(index.read(offset).is_err());
    }

    #[test]
    fn private_record_directory_is_removed_on_drop() {
        let index = DiskDirectoryIndex::new().unwrap();
        let path = index.scratch.path().to_path_buf();
        assert!(path.is_dir());

        drop(index);

        assert!(!path.exists());
    }

    #[test]
    fn external_order_supports_random_ranges() {
        let provider = ProviderId::new("local").unwrap();
        let mut index = DiskDirectoryIndex::new().unwrap();
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
    }

    #[test]
    fn identity_lookup_reaches_offscreen_records() {
        let provider = ProviderId::new("local").unwrap();
        let mut index = DiskDirectoryIndex::new().unwrap();
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
    fn later_record_replaces_same_identity_in_visible_order_and_lookup() {
        let mut index = DiskDirectoryIndex::new().unwrap();
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
        let mut index = DiskDirectoryIndex::new().unwrap();
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
        let mut index = DiskDirectoryIndex::new().unwrap();
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
        assert_eq!(duplicate_ids[1], ItemId::new(provider, vec![5]).unwrap());

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
    }

    #[test]
    #[ignore = "resource-intensive million-item index verification"]
    fn external_order_supports_one_million_records() {
        let provider = ProviderId::new("local").unwrap();
        let mut index = DiskDirectoryIndex::new().unwrap();
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
}
