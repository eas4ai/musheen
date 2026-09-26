use super::*;
use crate::Locale;
use gpui_kit::{BackgroundExecutor, WeakEntity};
use musheen_core::CapabilityMatrix;
use musheen_desktop::{
    BackendTagService, HomeItemKind, HomeSection, MountShortcut, TagBackend, TagService,
    TagStorage, XattrTagBackend,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_OBSERVED_SCOPES: usize = 16;
const MAX_OBSERVED_ITEMS_PER_SCOPE: usize = 4_096;
const MAX_XATTR_RECONCILIATION_WRITES: usize = 32;
/// The most folder identities the window keeps; the cache starts over when
/// full.
const FOLDER_IDENTITIES_LIMIT: usize = 1_024;
/// The most recorded navigations kept to order a late recent below the
/// ones after it: more than the catalog keeps recents.
const RECENT_CLAIMS_LIMIT: usize = 64;
/// The most pins the window asks the store about at once. A pin on a store
/// that never answers holds one background thread, so this bounds the
/// threads pins can hold.
const PIN_PROBES_LIMIT: usize = 4;
type ObservedScopes = Vec<(StorePath, BTreeMap<ItemId, StorePath>)>;
/// Work that waits for folder identities, run once the store answered.
pub(super) type FolderIdentityWaiter = Box<dyn FnOnce(&mut MusheenApp, &mut Context<MusheenApp>)>;

#[cfg(test)]
thread_local! {
    /// Whether this thread runs a queued catalog write, so a test can count
    /// the writes that did not go through the queue.
    static IN_WRITE_JOB: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread runs a queued catalog write now.
#[cfg(test)]
pub(super) fn in_write_job() -> bool {
    IN_WRITE_JOB.get()
}

/// Reads and writes a file's extended-attribute tags. The app uses the
/// system's attributes; tests keep them in memory.
pub(super) trait AttributeTags: std::fmt::Debug + Send + Sync {
    fn read(&self, item: &ItemId, path: &StorePath) -> Result<BTreeSet<Box<str>>, Box<str>>;
    fn write(
        &self,
        item: &ItemId,
        path: &StorePath,
        tags: &BTreeSet<Box<str>>,
    ) -> Result<(), Box<str>>;
}

/// The system's extended attributes.
#[derive(Debug)]
struct SystemAttributeTags;

impl AttributeTags for SystemAttributeTags {
    fn read(&self, item: &ItemId, path: &StorePath) -> Result<BTreeSet<Box<str>>, Box<str>> {
        BackendTagService::new(XattrTagBackend::new(true, true))
            .tags(item, path)
            .map_err(|error| Box::<str>::from(error.to_string()))
    }

    fn write(
        &self,
        item: &ItemId,
        path: &StorePath,
        tags: &BTreeSet<Box<str>>,
    ) -> Result<(), Box<str>> {
        XattrTagBackend::new(true, true)
            .write_tags(item, path, tags)
            .map_err(|error| Box::<str>::from(error.to_string()))
    }
}

#[derive(Clone, Debug)]
pub(super) struct CatalogBinding {
    store: Option<CatalogStore>,
    document: Arc<Mutex<CatalogDocument>>,
    revision: Arc<AtomicU64>,
    observed_scopes: Arc<Mutex<ObservedScopes>>,
    /// Serializes updates without making a reader of the in-memory copy
    /// wait on the catalog file's lock.
    update_lock: Arc<Mutex<()>>,
    /// Writes queued by every window that shares this catalog, run in order.
    writes: CatalogWriteQueue,
    /// Keeps recent locations in navigation order for every window that
    /// shares this catalog.
    recents_order: Arc<Mutex<RecentsOrder>>,
    /// Where files' extended-attribute tags are read and written.
    attribute_tags: Arc<dyn AttributeTags>,
    xattr_opt_in: bool,
    /// Catalog writes that ran outside the write queue.
    #[cfg(test)]
    writes_outside_queue: Arc<std::sync::atomic::AtomicUsize>,
}

/// Navigation order for recent locations. A navigation's recent is written
/// once the store answers for its folder, so a later navigation can answer
/// first; its recent then goes below the later ones. A navigation made
/// before a clear records no recent.
#[derive(Debug, Default)]
struct RecentsOrder {
    /// The last sequence number given to a navigation.
    issued: u64,
    /// The last navigation before the latest clear.
    cleared_at: u64,
    /// The latest navigations whose recent was queued, with their folders.
    claimed: VecDeque<(u64, FolderIdentity)>,
}

/// Catalog writes run one at a time, in the order they were queued, on a
/// background executor. A queued write runs even when the window that
/// queued it closes first. Every catalog write goes through this queue, so
/// a catalog lock that another process holds keeps one background thread
/// waiting, not one per write.
#[derive(Clone, Default)]
pub(super) struct CatalogWriteQueue {
    state: Arc<Mutex<CatalogWriteQueueState>>,
}

#[derive(Default)]
struct CatalogWriteQueueState {
    jobs: VecDeque<Box<dyn FnOnce() + Send>>,
    draining: bool,
    /// Dropped when the queue runs empty, which wakes their receivers.
    settled: Vec<async_channel::Sender<()>>,
}

impl std::fmt::Debug for CatalogWriteQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        formatter
            .debug_struct("CatalogWriteQueue")
            .field("queued", &state.jobs.len())
            .field("draining", &state.draining)
            .finish()
    }
}

impl CatalogWriteQueue {
    /// Queues `job`. Returns whether no drain runs, so the caller starts one.
    fn push(&self, job: Box<dyn FnOnce() + Send>) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.jobs.push_back(job);
        !std::mem::replace(&mut state.draining, true)
    }

    /// Runs the queued jobs until none is left. A job that panics is dropped
    /// and the next one runs.
    fn drain(&self) {
        loop {
            let job = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(job) = state.jobs.pop_front() else {
                    state.draining = false;
                    state.settled.clear();
                    return;
                };
                job
            };
            #[cfg(test)]
            IN_WRITE_JOB.set(true);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            #[cfg(test)]
            IN_WRITE_JOB.set(false);
        }
    }

    /// Resolves once every job queued so far has run.
    fn settled(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let receiver = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.draining.then(|| {
                let (sender, receiver) = async_channel::bounded::<()>(1);
                state.settled.push(sender);
                receiver
            })
        };
        async move {
            if let Some(receiver) = receiver {
                let _ = receiver.recv().await;
            }
        }
    }
}

pub(super) type TagTarget = (ItemId, StorePath, CapabilityMatrix);
pub(super) type TagStates = (BTreeSet<Box<str>>, BTreeSet<Box<str>>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DirectoryObservation {
    Partial,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrphanCleanupOutcome {
    Removed,
    StillPresent,
    NoLongerOrphaned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OrphanedTagRecord {
    pub(super) item: ItemId,
    pub(super) path: StorePath,
    pub(super) tags: BTreeSet<Box<str>>,
}

impl CatalogBinding {
    pub(super) fn in_memory() -> Self {
        Self::in_memory_with_xattr_opt_in(false)
    }

    pub(super) fn in_memory_with_xattr_opt_in(xattr_opt_in: bool) -> Self {
        Self {
            store: None,
            document: Arc::new(Mutex::new(CatalogDocument::default())),
            revision: Arc::new(AtomicU64::new(0)),
            observed_scopes: Arc::new(Mutex::new(Vec::new())),
            update_lock: Arc::new(Mutex::new(())),
            writes: CatalogWriteQueue::default(),
            recents_order: Arc::default(),
            attribute_tags: Arc::new(SystemAttributeTags),
            xattr_opt_in,
            #[cfg(test)]
            writes_outside_queue: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn persistent(store: CatalogStore, document: CatalogDocument) -> Self {
        Self::persistent_with_xattr_opt_in(store, document, false)
    }

    pub(super) fn persistent_with_xattr_opt_in(
        store: CatalogStore,
        document: CatalogDocument,
        xattr_opt_in: bool,
    ) -> Self {
        Self {
            store: Some(store),
            document: Arc::new(Mutex::new(document)),
            revision: Arc::new(AtomicU64::new(0)),
            observed_scopes: Arc::new(Mutex::new(Vec::new())),
            update_lock: Arc::new(Mutex::new(())),
            writes: CatalogWriteQueue::default(),
            recents_order: Arc::default(),
            attribute_tags: Arc::new(SystemAttributeTags),
            xattr_opt_in,
            #[cfg(test)]
            writes_outside_queue: Arc::default(),
        }
    }

    /// This binding with its extended-attribute tags kept by `tags`.
    #[cfg(test)]
    pub(super) fn with_attribute_tags(mut self, tags: Arc<dyn AttributeTags>) -> Self {
        self.attribute_tags = tags;
        self
    }

    pub(super) fn snapshot(&self) -> CatalogDocument {
        self.snapshot_with_revision().0
    }

    fn snapshot_with_revision(&self) -> (CatalogDocument, u64) {
        let document = self.document.lock().expect("catalog lock is not poisoned");
        (document.clone(), self.revision.load(Ordering::Acquire))
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Queues `work` behind the writes every window sharing this catalog
    /// queued before it, and starts a drain on `executor` when none runs.
    /// The receiver hears the result; it closes without one when the work
    /// panicked.
    pub(super) fn queue_work<R: Send + 'static>(
        &self,
        executor: &BackgroundExecutor,
        work: impl FnOnce() -> R + Send + 'static,
    ) -> async_channel::Receiver<R> {
        let (sender, receiver) = async_channel::bounded(1);
        let job = Box::new(move || {
            let _ = sender.try_send(work());
        });
        if self.writes.push(job) {
            let binding = self.clone();
            executor
                .spawn(async move { binding.writes.drain() })
                .detach();
        }
        receiver
    }

    /// Resolves once every write queued so far has run.
    pub(super) fn writes_settled(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        self.writes.settled()
    }

    /// How many catalog writes ran outside the write queue.
    #[cfg(test)]
    pub(super) fn writes_outside_queue(&self) -> usize {
        self.writes_outside_queue
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A sequence number for a navigation that records a recent location.
    pub(super) fn next_recent_sequence(&self) -> u64 {
        let mut order = self
            .recents_order
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        order.issued += 1;
        order.issued
    }

    /// Claims navigation `sequence`'s recent for `identity`. `None` when a
    /// clear came after the navigation or a later navigation already
    /// recorded the folder; otherwise the folders later navigations
    /// recorded, which stay above it.
    pub(super) fn claim_recent(
        &self,
        sequence: u64,
        identity: &FolderIdentity,
    ) -> Option<Vec<FolderIdentity>> {
        let mut order = self
            .recents_order
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if sequence <= order.cleared_at {
            return None;
        }
        let newer = order
            .claimed
            .iter()
            .filter(|(claimed, _)| *claimed > sequence)
            .map(|(_, newer)| newer.clone())
            .collect::<Vec<_>>();
        if newer.contains(identity) {
            return None;
        }
        order.claimed.push_back((sequence, identity.clone()));
        if order.claimed.len() > RECENT_CLAIMS_LIMIT {
            order.claimed.pop_front();
        }
        Some(newer)
    }

    /// Notes a clear of the recent locations: the navigations made before it
    /// no longer record theirs.
    pub(super) fn note_recents_cleared(&self) {
        let mut order = self
            .recents_order
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        order.cleared_at = order.issued;
    }

    fn remember_directory_observation(
        &self,
        location: &StorePath,
        items: &[StoreItem],
        observation: DirectoryObservation,
    ) {
        let mut scopes = self
            .observed_scopes
            .lock()
            .expect("observed-scope lock is not poisoned");
        let mut entries = scopes
            .iter()
            .position(|(scope, _)| scope == location)
            .map(|index| scopes.remove(index).1)
            .unwrap_or_default();
        if observation == DirectoryObservation::Complete {
            entries.clear();
        }
        for item in items.iter().rev().take(MAX_OBSERVED_ITEMS_PER_SCOPE) {
            entries.insert(item.id().clone(), item.path().clone());
        }
        while entries.len() > MAX_OBSERVED_ITEMS_PER_SCOPE {
            let Some(item) = entries.keys().next().cloned() else {
                break;
            };
            entries.remove(&item);
        }
        scopes.push((location.clone(), entries));
        if scopes.len() > MAX_OBSERVED_SCOPES {
            scopes.remove(0);
        }
    }

    fn observed_scope_for(&self, item: &ItemId, path: &StorePath) -> Option<StorePath> {
        self.observed_scopes
            .lock()
            .expect("observed-scope lock is not poisoned")
            .iter()
            .rev()
            .find_map(|(scope, entries)| (entries.get(item) == Some(path)).then(|| scope.clone()))
    }

    pub(super) fn update(&self, change: impl FnOnce(&mut CatalogDocument)) -> Result<(), Box<str>> {
        self.update_result(|document| {
            change(document);
            Ok(())
        })
    }

    fn update_result<T>(
        &self,
        change: impl FnOnce(&mut CatalogDocument) -> Result<T, Box<str>>,
    ) -> Result<T, Box<str>> {
        #[cfg(test)]
        if !IN_WRITE_JOB.get() {
            self.writes_outside_queue
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        // Updates run one at a time. The in-memory copy is locked only to
        // read or install a document, never while the catalog file's lock
        // is awaited, so a reader on the UI thread does not wait on another
        // process.
        let _serial = self
            .update_lock
            .lock()
            .expect("catalog update lock is not poisoned");
        if let Some(store) = &self.store {
            let previous_visible = self
                .document
                .lock()
                .expect("catalog lock is not poisoned")
                .clone();
            let update = store.update(|current| {
                let changed = change(current)?;
                Ok((changed, current.clone()))
            });
            let (changed, current) = match update {
                Ok(updated) => updated,
                Err(musheen_desktop::CatalogError::UpdateConflict) => {
                    let current = store
                        .load()
                        .map_err(|error| Box::<str>::from(error.to_string()))?;
                    self.install_document(current, &previous_visible);
                    return Err(Box::<str>::from(
                        musheen_desktop::CatalogError::UpdateConflict.to_string(),
                    ));
                }
                Err(error) => {
                    return Err(match error {
                        musheen_desktop::CatalogError::Update(error) => error,
                        error => Box::<str>::from(error.to_string()),
                    });
                }
            };
            self.install_document(current, &previous_visible);
            return Ok(changed);
        }
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        let previous = document.clone();
        let changed = match change(&mut document) {
            Ok(changed) => changed,
            Err(error) => {
                *document = previous;
                return Err(error);
            }
        };
        if *document != previous {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        Ok(changed)
    }

    /// Installs the document the store now holds and counts a revision when
    /// it differs from what was visible before the update.
    fn install_document(&self, current: CatalogDocument, previous_visible: &CatalogDocument) {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        *document = current;
        if *document != *previous_visible {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[cfg(test)]
    pub(super) fn assign_tag(
        &self,
        item: &ItemId,
        path: &StorePath,
        capabilities: &CapabilityMatrix,
        tag: &str,
    ) -> Result<TagStorage, Box<str>> {
        TagService::validate_tag(tag).map_err(|error| Box::<str>::from(error.to_string()))?;
        let mut snapshot = self.snapshot();
        let storage = TagService::new(snapshot.tags_mut(), self.xattr_opt_in)
            .storage_for(path, capabilities)
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        self.apply_tag_delta(
            &[(item.clone(), path.clone(), capabilities.clone())],
            &[Box::<str>::from(tag.trim())].into_iter().collect(),
            &BTreeSet::new(),
        )?;
        Ok(storage)
    }

    pub(super) fn tags_for(
        &self,
        item: &ItemId,
        path: &StorePath,
        capabilities: &CapabilityMatrix,
    ) -> Result<BTreeSet<Box<str>>, Box<str>> {
        self.tags_for_with_xattr_io(
            item,
            path,
            capabilities,
            |item, path| self.attribute_tags.read(item, path),
            |item, path, tags| self.attribute_tags.write(item, path, tags),
        )
    }

    fn tags_for_with_xattr_io(
        &self,
        item: &ItemId,
        path: &StorePath,
        capabilities: &CapabilityMatrix,
        reader: impl FnOnce(&ItemId, &StorePath) -> Result<BTreeSet<Box<str>>, Box<str>>,
        mut writer: impl FnMut(&ItemId, &StorePath, &BTreeSet<Box<str>>) -> Result<(), Box<str>>,
    ) -> Result<BTreeSet<Box<str>>, Box<str>> {
        let mut snapshot = self.snapshot();
        let storage = TagService::new(snapshot.tags_mut(), self.xattr_opt_in)
            .storage_for(path, capabilities)
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        let catalog_tags = snapshot.tags().tags_for(item);
        if storage == TagStorage::AppCatalog {
            return Ok(catalog_tags);
        }
        if snapshot.tags().is_xattr_pending(item) {
            self.reconcile_pending_xattrs_with(writer)?;
            return Ok(self.snapshot().tags().tags_for(item));
        }

        let xattr_tags = reader(item, path)?;
        let mut merged = catalog_tags.clone();
        merged.extend(xattr_tags.iter().cloned());
        if merged == catalog_tags && merged == xattr_tags {
            return Ok(merged);
        }

        let expected_path = snapshot.tags().path_hint(item).cloned();
        let expected_orphaned = snapshot.tags().is_orphaned(item);
        let staged = self.update_result(|document| {
            let current_path = document.tags().path_hint(item).cloned();
            let current_tags = document.tags().tags_for(item);
            let current_pending = document.tags().is_xattr_pending(item);
            let current_orphaned = document.tags().is_orphaned(item);
            if current_path != expected_path
                || current_tags != catalog_tags
                || current_pending
                || current_orphaned != expected_orphaned
            {
                return Ok(false);
            }
            document
                .tags_mut()
                .stage_xattr_tags(item, path.clone(), merged.clone());
            Ok(true)
        })?;
        if !staged {
            return Err(Box::<str>::from(
                "catalog tags changed while importing extended attributes; retry the action",
            ));
        }
        self.reconcile_pending_xattrs_with(&mut writer)?;
        Ok(self.snapshot().tags().tags_for(item))
    }

    pub(super) fn tag_states(&self, targets: &[TagTarget]) -> Result<TagStates, Box<str>> {
        let Some((first_item, first_path, first_capabilities)) = targets.first() else {
            return Ok((BTreeSet::new(), BTreeSet::new()));
        };
        let mut common = self.tags_for(first_item, first_path, first_capabilities)?;
        let mut all = common.clone();
        for (item, path, capabilities) in targets.iter().skip(1) {
            let tags = self.tags_for(item, path, capabilities)?;
            common.retain(|tag| tags.contains(tag));
            all.extend(tags);
        }
        let mixed = all.difference(&common).cloned().collect();
        Ok((common, mixed))
    }

    pub(super) fn apply_tag_delta(
        &self,
        targets: &[TagTarget],
        added: &BTreeSet<Box<str>>,
        removed: &BTreeSet<Box<str>>,
    ) -> Result<(), Box<str>> {
        self.apply_tag_delta_with_xattr_writer(targets, added, removed, |item, path, tags| {
            self.attribute_tags.write(item, path, tags)
        })
    }

    fn apply_tag_delta_with_xattr_writer(
        &self,
        targets: &[TagTarget],
        added: &BTreeSet<Box<str>>,
        removed: &BTreeSet<Box<str>>,
        writer: impl FnMut(&ItemId, &StorePath, &BTreeSet<Box<str>>) -> Result<(), Box<str>>,
    ) -> Result<(), Box<str>> {
        for tag in added {
            TagService::validate_tag(tag).map_err(|error| Box::<str>::from(error.to_string()))?;
        }
        let mut prepared = Vec::with_capacity(targets.len());
        for (item, path, capabilities) in targets {
            let current = self.tags_for(item, path, capabilities)?;
            let mut snapshot = self.snapshot();
            let storage = TagService::new(snapshot.tags_mut(), self.xattr_opt_in)
                .storage_for(path, capabilities)
                .map_err(|error| Box::<str>::from(error.to_string()))?;
            let mut desired = current.clone();
            desired.retain(|tag| !removed.contains(tag));
            desired.extend(added.iter().cloned());
            prepared.push((
                item.clone(),
                path.clone(),
                storage,
                current,
                desired,
                self.observed_scope_for(item, path),
            ));
        }
        self.update_result(|document| {
            if prepared.iter().any(|(item, path, _, current, _, _)| {
                document.tags().tags_for(item) != *current
                    || document.tags().is_xattr_pending(item)
                    || document
                        .tags()
                        .path_hint(item)
                        .is_some_and(|current_path| current_path != path)
            }) {
                return Err(Box::<str>::from(
                    "catalog tags changed while preparing the update; retry the action",
                ));
            }
            for (item, path, storage, _, desired, scope_hint) in &prepared {
                let has_desired_tags = !desired.is_empty();
                if *storage == TagStorage::ExtendedAttribute {
                    document
                        .tags_mut()
                        .stage_xattr_tags(item, path.clone(), desired.clone());
                } else {
                    document
                        .tags_mut()
                        .write_tags(item, path, desired)
                        .expect("the app-owned tag catalog is infallible");
                }
                if has_desired_tags && let Some(scope_hint) = scope_hint {
                    document.tags_mut().observe_present_in_scope(
                        item,
                        path.clone(),
                        scope_hint.clone(),
                    );
                }
            }
            Ok(())
        })?;
        self.reconcile_pending_xattrs_with(writer)
    }

    fn reconcile_pending_xattrs_with(
        &self,
        mut writer: impl FnMut(&ItemId, &StorePath, &BTreeSet<Box<str>>) -> Result<(), Box<str>>,
    ) -> Result<(), Box<str>> {
        let pending = {
            let document = self.document.lock().expect("catalog lock is not poisoned");
            document
                .tags()
                .pending_xattr_records()
                .map(|(item, path, tags)| (item.clone(), path.clone(), tags.clone()))
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for (item, initial_path, initial_tags) in pending {
            let mut desired = (initial_path, initial_tags);
            let mut converged = false;
            for _ in 0..MAX_XATTR_RECONCILIATION_WRITES {
                if let Err(error) = writer(&item, &desired.0, &desired.1) {
                    first_error.get_or_insert(error);
                    break;
                }
                let next = self.update_result(|document| {
                    let current = document.tags().path_hint(&item).cloned().map(|path| {
                        let tags = document.tags().tags_for(&item);
                        let pending = document.tags().is_xattr_pending(&item);
                        (path, tags, pending)
                    });
                    match current {
                        Some((path, tags, pending)) if path == desired.0 && tags == desired.1 => {
                            if pending {
                                document.tags_mut().finish_xattr_reconciliation(&item);
                            }
                            Ok(None)
                        }
                        Some((path, tags, _)) => {
                            document
                                .tags_mut()
                                .stage_xattr_tags(&item, path.clone(), tags.clone());
                            Ok(Some((path, tags)))
                        }
                        None => {
                            let path = desired.0.clone();
                            let tags = BTreeSet::new();
                            document
                                .tags_mut()
                                .stage_xattr_tags(&item, path.clone(), tags.clone());
                            Ok(Some((path, tags)))
                        }
                    }
                })?;
                let Some(next) = next else {
                    converged = true;
                    break;
                };
                desired = next;
            }
            if !converged && first_error.is_none() {
                first_error = Some(Box::<str>::from(
                    "tag metadata changed too often to reconcile extended attributes safely",
                ));
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn reconcile_pending_xattrs(&self) -> Result<(), Box<str>> {
        self.reconcile_pending_xattrs_with(|item, path, tags| {
            self.attribute_tags.write(item, path, tags)
        })
    }

    #[cfg(test)]
    pub(super) fn tags_for_identity(&self, item: &ItemId) -> BTreeSet<Box<str>> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in).tags_for(item)
    }

    pub(super) fn tag_names(&self) -> BTreeSet<Box<str>> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in).tag_names()
    }

    pub(super) fn tag_name_from_target(
        &self,
        target: &CommandTargetRef,
    ) -> Result<Box<str>, Box<str>> {
        let Some((provider, key)) = target.path().provider_key() else {
            return Err("tag actions require an exact tag shortcut identity".into());
        };
        if provider.as_str() != "musheen-tag"
            || target.id().provider() != provider
            || target.id().opaque_key() != key
        {
            return Err("the captured tag identity is invalid".into());
        }
        let name = std::str::from_utf8(key)
            .map_err(|_| Box::<str>::from("the captured tag name is not valid UTF-8"))?;
        self.tag_names()
            .contains(name)
            .then(|| Box::<str>::from(name))
            .ok_or_else(|| Box::<str>::from("the captured tag no longer exists"))
    }

    pub(super) fn rename_tag(
        &self,
        old: &str,
        new: &str,
        mut capabilities: impl FnMut(&StorePath) -> CapabilityMatrix,
    ) -> Result<usize, Box<str>> {
        TagService::validate_tag(new).map_err(|error| Box::<str>::from(error.to_string()))?;
        let document = self.snapshot();
        let targets = document
            .tags()
            .tracked_items()
            .filter(|(item, _, orphaned)| {
                !*orphaned && document.tags().tags_for(item).contains(old)
            })
            .map(|(item, path, _)| (item.clone(), path.clone(), capabilities(path)))
            .collect::<Vec<_>>();
        let changed = document
            .tags()
            .tracked_items()
            .filter(|(item, _, _)| document.tags().tags_for(item).contains(old))
            .count();
        self.apply_tag_delta(
            &targets,
            &[Box::<str>::from(new.trim())].into_iter().collect(),
            &[Box::<str>::from(old)].into_iter().collect(),
        )?;
        self.update_result(|document| {
            document
                .tags_mut()
                .rename(old, new)
                .map_err(|error| Box::<str>::from(error.to_string()))?;
            Ok(())
        })?;
        Ok(changed)
    }

    pub(super) fn delete_tag(
        &self,
        tag: &str,
        mut capabilities: impl FnMut(&StorePath) -> CapabilityMatrix,
    ) -> Result<usize, Box<str>> {
        let document = self.snapshot();
        let targets = document
            .tags()
            .tracked_items()
            .filter(|(item, _, orphaned)| {
                !*orphaned && document.tags().tags_for(item).contains(tag)
            })
            .map(|(item, path, _)| (item.clone(), path.clone(), capabilities(path)))
            .collect::<Vec<_>>();
        let changed = document
            .tags()
            .tracked_items()
            .filter(|(item, _, _)| document.tags().tags_for(item).contains(tag))
            .count();
        self.apply_tag_delta(
            &targets,
            &BTreeSet::new(),
            &[Box::<str>::from(tag)].into_iter().collect(),
        )?;
        self.update(|document| {
            document.tags_mut().delete(tag);
        })?;
        Ok(changed)
    }

    /// The paths of the live items that carry `tag`: the paths whose
    /// capabilities a rename or a delete of the tag asks the store for.
    pub(super) fn tagged_paths(&self, tag: &str) -> Vec<StorePath> {
        let document = self.snapshot();
        document
            .tags()
            .tracked_items()
            .filter(|(item, _, orphaned)| {
                !*orphaned && document.tags().tags_for(item).contains(tag)
            })
            .map(|(_, path, _)| path.clone())
            .collect()
    }

    pub(super) fn items_with_tag(&self, tag: &str) -> BTreeSet<ItemId> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in).items_with_tag(tag)
    }

    pub(super) fn complete_move(
        &self,
        source: &ItemId,
        destination: ItemId,
        destination_path: StorePath,
        destination_capabilities: &CapabilityMatrix,
    ) -> Result<TagMoveOutcome, Box<str>> {
        if source == &destination {
            return self.complete_rename(source, destination_path, destination_capabilities);
        }
        self.update_result(|document| {
            Ok(document.note_completed_move(
                source,
                destination,
                destination_path,
                destination_capabilities,
            ))
        })
    }

    pub(super) fn complete_rename(
        &self,
        item: &ItemId,
        destination_path: StorePath,
        destination_capabilities: &CapabilityMatrix,
    ) -> Result<TagMoveOutcome, Box<str>> {
        self.update_result(|document| {
            Ok(document.note_completed_rename(item, destination_path, destination_capabilities))
        })
    }

    pub(super) fn observe_present(&self, item: &ItemId, path: StorePath) -> Result<(), Box<str>> {
        self.update(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in).observe_present(item, path);
        })
    }

    #[cfg(test)]
    pub(super) fn observe_missing(&self, item: &ItemId) -> Result<(), Box<str>> {
        self.update(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in).observe_missing(item);
        })
    }

    /// Applies a batch of watch observations in one catalog update: the
    /// catalog file is locked and rewritten once for the batch.
    pub(super) fn observe_batch(
        &self,
        observations: Vec<super::CatalogObservation>,
    ) -> Result<(), Box<str>> {
        self.update(|document| {
            for observation in observations {
                match observation {
                    super::CatalogObservation::Present(id, path) => {
                        TagService::new(document.tags_mut(), self.xattr_opt_in)
                            .observe_present(&id, path);
                    }
                    super::CatalogObservation::Missing(id) => {
                        TagService::new(document.tags_mut(), self.xattr_opt_in)
                            .observe_missing(&id);
                    }
                }
            }
        })
    }

    #[cfg(test)]
    pub(super) fn cleanup_reviewed_orphans<'a>(
        &self,
        reviewed: impl IntoIterator<Item = &'a ItemId>,
    ) -> Result<usize, Box<str>> {
        self.update_result(|document| {
            Ok(TagService::new(document.tags_mut(), self.xattr_opt_in)
                .cleanup_reviewed_orphans(reviewed))
        })
    }

    #[cfg(test)]
    pub(super) fn cleanup_reviewed_orphan(
        &self,
        reviewed: &ItemId,
        mut resolve: impl FnMut(&StorePath) -> Result<Option<StoreItem>, Box<str>>,
    ) -> Result<OrphanCleanupOutcome, Box<str>> {
        let Some(path) = self.orphan_path(reviewed)? else {
            return Ok(OrphanCleanupOutcome::NoLongerOrphaned);
        };
        self.finish_orphan_cleanup(reviewed, &path, resolve(&path))
    }

    /// The path hint of a reviewed orphan, or `None` when the item is no
    /// longer orphaned.
    pub(super) fn orphan_path(&self, reviewed: &ItemId) -> Result<Option<StorePath>, Box<str>> {
        let snapshot = self.snapshot();
        if !snapshot.tags().is_orphaned(reviewed) {
            return Ok(None);
        }
        snapshot
            .tags()
            .path_hint(reviewed)
            .cloned()
            .map(Some)
            .ok_or_else(|| Box::<str>::from("the reviewed tag record has no path hint"))
    }

    /// Removes a reviewed orphan's tags unless `found`, the store's answer
    /// for `path`, is the item itself. The record must still be the orphan
    /// at `path`.
    pub(super) fn finish_orphan_cleanup(
        &self,
        reviewed: &ItemId,
        path: &StorePath,
        found: Result<Option<StoreItem>, Box<str>>,
    ) -> Result<OrphanCleanupOutcome, Box<str>> {
        if let Some(item) = found?
            && item.id() == reviewed
        {
            self.observe_present(reviewed, item.path().clone())?;
            return Ok(OrphanCleanupOutcome::StillPresent);
        }
        self.update_result(|document| {
            if !document.tags().is_orphaned(reviewed)
                || document.tags().path_hint(reviewed) != Some(path)
            {
                return Ok(OrphanCleanupOutcome::NoLongerOrphaned);
            }
            let removed = TagService::new(document.tags_mut(), self.xattr_opt_in)
                .cleanup_reviewed_orphans([reviewed]);
            Ok(if removed == 1 {
                OrphanCleanupOutcome::Removed
            } else {
                OrphanCleanupOutcome::NoLongerOrphaned
            })
        })
    }

    #[cfg(test)]
    pub(super) fn path_hint(&self, item: &ItemId) -> Option<StorePath> {
        self.snapshot().tags().path_hint(item).cloned()
    }

    #[cfg(test)]
    pub(super) fn is_orphaned(&self, item: &ItemId) -> bool {
        self.snapshot().tags().is_orphaned(item)
    }

    pub(super) fn home_sections(&self, mounts: &[MountShortcut]) -> Vec<HomeSection> {
        self.snapshot().home_sections(mounts)
    }

    pub(super) fn orphaned_tags(&self) -> Vec<OrphanedTagRecord> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in)
            .orphaned_items()
            .map(|(item, path, tags)| OrphanedTagRecord {
                item: item.clone(),
                path: path.clone(),
                tags: tags.clone(),
            })
            .collect()
    }

    /// Checks every pin in turn. The app asks the store about each pin on
    /// its own and records each answer with `record_pin_resolution`.
    #[cfg(test)]
    pub(super) fn reconcile_pins(
        &self,
        mut resolve: impl FnMut(&StorePath) -> Result<Option<StoreItem>, Box<str>>,
    ) -> Result<(), Box<str>> {
        for pin in self.snapshot().pins().entries() {
            self.record_pin_resolution(pin.item(), pin.path_hint(), resolve(pin.path_hint()))?;
        }
        Ok(())
    }

    /// Records the store's answer for one pin, while the pin still has the
    /// path the store was asked about.
    pub(super) fn record_pin_resolution(
        &self,
        item: &ItemId,
        expected_path: &StorePath,
        found: Result<Option<StoreItem>, Box<str>>,
    ) -> Result<(), Box<str>> {
        let resolution = match found {
            Ok(Some(found)) if found.id() == item => Ok(found.path().clone()),
            Ok(Some(_)) => Err(Box::<str>::from(
                "the stored path now identifies a different item",
            )),
            Ok(None) => Err(Box::<str>::from("the target is missing")),
            Err(error) => Err(error),
        };
        self.update(|document| {
            if !document
                .pins()
                .entries()
                .iter()
                .any(|pin| pin.item() == item && pin.path_hint() == expected_path)
            {
                return;
            }
            match resolution {
                Ok(path) => {
                    document.pins_mut().mark_available(item, path);
                }
                Err(reason) => {
                    document.pins_mut().mark_unavailable(item, reason);
                }
            }
        })
    }

    pub(super) fn reconcile_directory(
        &self,
        location: &StorePath,
        items: &[StoreItem],
        observation: DirectoryObservation,
    ) -> Result<(), Box<str>> {
        self.remember_directory_observation(location, items, observation);
        let present = items
            .iter()
            .map(|item| item.id().clone())
            .collect::<BTreeSet<_>>();
        self.update(|document| {
            let tracked = TagService::new(document.tags_mut(), self.xattr_opt_in)
                .tracked_scoped_items()
                .map(|(item, path, scope, _)| (item.clone(), path.clone(), scope.cloned()))
                .collect::<Vec<_>>();
            let mut service = TagService::new(document.tags_mut(), self.xattr_opt_in);
            for item in items {
                service.observe_present_in_scope(item.id(), item.path().clone(), location.clone());
            }
            if observation != DirectoryObservation::Complete {
                return;
            }
            for (item, path, scope) in tracked {
                let observed_in_scope = scope.as_ref() == Some(location)
                    || scope.is_none()
                        && location.as_unix_path().is_some_and(|directory| {
                            path.as_unix_path()
                                .and_then(std::path::Path::parent)
                                .is_some_and(|parent| parent == directory)
                        });
                if observed_in_scope && !present.contains(&item) {
                    service.observe_missing(&item);
                }
            }
        })
    }
}

impl MusheenApp {
    /// Runs catalog work on a background executor after the work queued
    /// before it, then applies its result on the UI thread. The catalog
    /// file's lock is taken there, never on the UI thread, and two edits of
    /// the same entry land in the order they were made. Work that asks the
    /// store runs the store call first and queues only the write, so a
    /// store that blocks never holds the queue. The work runs even when
    /// this window closes first; `then` runs only while it is open, and
    /// hears an error when the work stopped without a result.
    pub(super) fn queue_catalog_work<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T, Box<str>> + Send + 'static,
        then: impl FnOnce(&mut Self, Result<T, Box<str>>, &mut Context<Self>) + 'static,
    ) {
        let written = self
            .catalog_binding
            .queue_work(cx.background_executor(), work);
        cx.spawn(async move |this, cx| {
            let result = catalog_work_result(written).await;
            let _ = this.update(cx, |state, cx| then(state, result, cx));
        })
        .detach();
    }

    /// Lets the queued catalog writes finish when the app quits, for as long
    /// as GPUI waits on its quit handlers.
    pub(super) fn settle_catalog_writes_on_quit(&mut self, cx: &mut Context<Self>) {
        cx.on_app_quit(|state, _| state.catalog_binding.writes_settled())
            .detach();
    }

    /// Shows a failed catalog write on the error line.
    pub(super) fn report_catalog_error(
        &mut self,
        result: Result<(), Box<str>>,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = result {
            self.operation_error = Some(error);
            cx.notify();
        }
    }

    pub(super) fn clear_recent_locations(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<str>> {
        // A navigation made before the clear whose folder the store has not
        // answered for yet records no recent after it.
        self.catalog_binding.note_recents_cleared();
        let binding = self.catalog_binding.clone();
        self.queue_catalog_work(
            cx,
            move || binding.update(|document| document.recents_mut().clear()),
            |state, result, cx| {
                match result {
                    Ok(()) => state.sync_catalog_projection(cx),
                    Err(error) => state.operation_error = Some(error),
                }
                cx.notify();
            },
        );
        Ok(())
    }

    /// The writer a Properties window saves its tag edits with. The edit is
    /// queued and `done` hears the result once the catalog took it.
    pub(super) fn properties_tag_writer(
        &self,
        tag_targets: Vec<TagTarget>,
        cx: &mut Context<Self>,
    ) -> TagWriter {
        let app = cx.entity().downgrade();
        let writer: TagWriter = Arc::new(
            move |desired: &TagDelta, cx: &mut App, done: TagWriteDone| {
                let mut done = Some(done);
                let targets = tag_targets.clone();
                let delta = desired.clone();
                let queued = app.update(cx, |state, cx| {
                    if let Some(done) = done.take() {
                        state.apply_properties_tags(targets, delta, done, cx);
                    }
                });
                if let (Err(error), Some(done)) = (queued, done.take()) {
                    done(Err(error.to_string().into()), cx);
                }
            },
        );
        writer
    }

    pub(super) fn apply_properties_tags(
        &mut self,
        targets: Vec<TagTarget>,
        delta: TagDelta,
        done: TagWriteDone,
        cx: &mut Context<Self>,
    ) {
        let binding = self.catalog_binding.clone();
        let written = self
            .catalog_binding
            .queue_work(cx.background_executor(), move || {
                binding.apply_tag_delta(&targets, &delta.added, &delta.removed)
            });
        // The Properties window outlives the window that opened it, so
        // `done` hears the result whether or not this window is still open.
        cx.spawn(async move |this, cx| {
            let result = catalog_work_result(written).await;
            if result.is_ok() {
                let _ = this.update(cx, |state, cx| {
                    state.sync_catalog_projection(cx);
                    cx.notify();
                });
            }
            cx.update(|cx| done(result, cx));
        })
        .detach();
    }

    /// Asks the store for the capabilities of the paths that carry `tag`,
    /// then queues `change`, which reads and writes the catalog inside the
    /// write queue. A path tagged after the probe is asked inside the job.
    /// The task gives the receiver of the change's result.
    fn queue_tag_change(
        &self,
        tag: &str,
        change: impl FnOnce(
            &CatalogBinding,
            &mut dyn FnMut(&StorePath) -> CapabilityMatrix,
        ) -> Result<usize, Box<str>>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Task<async_channel::Receiver<Result<usize, Box<str>>>> {
        let paths = self.catalog_binding.tagged_paths(tag);
        let store = Arc::clone(&self.store);
        let binding = self.catalog_binding.clone();
        let executor = cx.background_executor().clone();
        cx.background_spawn(async move {
            let probed = paths
                .into_iter()
                .map(|path| {
                    let capabilities = store.capabilities(&path);
                    (path, capabilities)
                })
                .collect::<HashMap<_, _>>();
            let writer = binding.clone();
            binding.queue_work(&executor, move || {
                change(&writer, &mut |path: &StorePath| {
                    probed
                        .get(path)
                        .cloned()
                        .unwrap_or_else(|| store.capabilities(path))
                })
            })
        })
    }

    /// Deletes a sidebar tag. The tagged paths' capabilities are asked off
    /// the UI thread, then the delete is queued; a failure shows on the
    /// error line.
    pub(super) fn delete_captured_tag(
        &mut self,
        target: &CommandTargetRef,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<str>> {
        let tag = self.catalog_binding.tag_name_from_target(target)?;
        let deleted = tag.clone();
        let written = self.queue_tag_change(
            &tag,
            move |binding, capabilities| binding.delete_tag(&deleted, capabilities),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = catalog_work_result(written.await).await;
            let _ = this.update(cx, |state, cx| {
                match result {
                    Ok(_) => state.sync_catalog_projection(cx),
                    Err(error) => state.operation_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        Ok(())
    }

    /// Renames a sidebar tag. The tagged paths' capabilities are asked off
    /// the UI thread, then the rename is queued; `done` hears the result.
    pub(super) fn rename_catalog_tag(
        &mut self,
        old: &str,
        new: &str,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<(), Box<str>>, &mut App) + 'static,
    ) {
        let (renamed, name) = (old.to_owned(), new.to_owned());
        let written = self.queue_tag_change(
            old,
            move |binding, capabilities| binding.rename_tag(&renamed, &name, capabilities),
            cx,
        );
        // The rename window outlives the window that opened it, so `done`
        // hears the result whether or not this window is still open.
        cx.spawn(async move |this, cx| {
            let result = catalog_work_result(written.await)
                .await
                .and_then(|changed| {
                    if changed == 0 {
                        Err(Box::<str>::from("the captured tag no longer exists"))
                    } else {
                        Ok(())
                    }
                });
            if result.is_ok() {
                let _ = this.update(cx, |state, cx| {
                    state.sync_catalog_projection(cx);
                    cx.notify();
                });
            }
            cx.update(|cx| done(result, cx));
        })
        .detach();
    }

    pub(super) fn open_captured_tag_rename(
        &mut self,
        target: &CommandTargetRef,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<str>> {
        let tag = self.catalog_binding.tag_name_from_target(target)?;
        let app = cx.entity().downgrade();
        let title = self
            .catalog
            .message("catalog-rename-tag")
            .expect("the rename-tag catalog message exists")
            .to_owned();
        let catalog = self.catalog.clone();
        let options = properties_window_options(title, cx);
        cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| TagRenameWindow::new(app, tag, catalog, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .map_err(|error| Box::<str>::from(error.to_string()))?;
        Ok(())
    }

    /// Checks the pins against the store off the UI thread and shows the
    /// catalog as it is now.
    pub(super) fn sync_catalog_projection(&mut self, cx: &mut Context<Self>) {
        self.reconcile_pins(cx);
        self.project_catalog();
    }

    /// Asks the store whether each pin still resolves, on a background
    /// executor, and queues each answer's catalog write as it comes. A pin
    /// is asked once at a time and at most `PIN_PROBES_LIMIT` pins at once,
    /// so a pin on a store that never answers holds one background thread
    /// and the other pins are still checked.
    pub(super) fn reconcile_pins(&mut self, cx: &mut Context<Self>) {
        for pin in self.catalog_binding.snapshot().pins().entries() {
            let key = (pin.item().clone(), pin.path_hint().clone());
            if !self.pin_probes.contains(&key) && !self.pins_waiting.contains(&key) {
                self.pins_waiting.push_back(key);
            }
        }
        self.start_pin_probes(cx);
    }

    /// Starts probes for the waiting pins while fewer than
    /// `PIN_PROBES_LIMIT` run.
    fn start_pin_probes(&mut self, cx: &mut Context<Self>) {
        while self.pin_probes.len() < PIN_PROBES_LIMIT {
            let Some((item, path)) = self.pins_waiting.pop_front() else {
                return;
            };
            let key = (item.clone(), path.clone());
            self.pin_probes.insert(key.clone());
            let store = Arc::clone(&self.store);
            let binding = self.catalog_binding.clone();
            let executor = cx.background_executor().clone();
            let probe = cx.background_spawn(async move {
                let found = store
                    .resolve_item(&path)
                    .map_err(|error| Box::<str>::from(error.to_string()));
                let writer = binding.clone();
                binding.queue_work(&executor, move || {
                    writer.record_pin_resolution(&item, &path, found)
                })
            });
            cx.spawn(async move |this, cx| {
                let written = probe.await;
                // The probe's thread is free again, so the next pin starts.
                let open = this.update(cx, |state, cx| {
                    state.pin_probes.remove(&key);
                    state.start_pin_probes(cx);
                });
                if open.is_err() {
                    return;
                }
                let result = catalog_work_result(written).await;
                let _ = this.update(cx, |state, cx| {
                    if let Err(error) = result {
                        state.operation_error = Some(error);
                    }
                    if state.catalog_binding.revision() != state.catalog_projection_revision {
                        state.project_catalog();
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Shows the catalog's current document: pins, tag names, tag filters
    /// and each tab's folder view preferences. Reads memory only.
    pub(super) fn project_catalog(&mut self) {
        let (document, projection_revision) = self.catalog_binding.snapshot_with_revision();
        self.pins.replace_catalog(document.pins());
        let tag_names = document.tags().tag_names();
        for sidebar in self.sidebars.values_mut() {
            sidebar.set_tag_names(tag_names.iter().map(AsRef::as_ref));
        }
        for active in self.filters.values_mut() {
            let Some(tag) = active
                .expression
                .strip_prefix("tag:")
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
            else {
                continue;
            };
            active.filter = Some(
                DirectoryFilter::default().with_tagged_items(document.tags().items_with_tag(tag)),
            );
            active.error = None;
        }
        self.apply_catalog_view_preferences(&document, None);
        self.catalog_projection_revision = projection_revision;
    }

    /// Gives each tab its folder's view preferences from `document`; with
    /// `only`, just the tabs at those locations.
    fn apply_catalog_view_preferences(
        &mut self,
        document: &CatalogDocument,
        only: Option<&std::collections::HashSet<StorePath>>,
    ) {
        let locations = self
            .navigation
            .panes()
            .iter()
            .flat_map(|pane| pane.tabs())
            .filter(|tab| only.is_none_or(|only| only.contains(tab.location())))
            .map(|tab| {
                (
                    tab.id(),
                    tab.location().clone(),
                    tab.view_preferences().clone(),
                )
            })
            .collect::<Vec<_>>();
        for (tab_id, location, base) in locations {
            let preferences = self.preferences_with_catalog(&location, base, document);
            if let Some(directory) = self.directories.get_mut(&tab_id) {
                *directory.view_mut().preferences_mut() = preferences.clone();
            }
            if let Some(tab) = self.navigation.tab_mut(tab_id) {
                tab.set_view_preferences(preferences);
            }
        }
    }

    /// The folder identity of `location` as the store last answered it, or
    /// the one the catalog remembers for the path. Never asks the store; a
    /// location the store has not answered for is resolved in the
    /// background by `with_folder_identities` or `request_folder_identities`.
    pub(super) fn folder_identity(&self, location: &StorePath) -> Option<FolderIdentity> {
        self.folder_identities
            .get(location)
            .cloned()
            .flatten()
            .or_else(|| {
                self.catalog_binding
                    .snapshot()
                    .folder_preferences()
                    .identity_for_path(location)
                    .cloned()
            })
    }

    fn parent_location(location: &StorePath) -> Option<StorePath> {
        let parent = location.as_unix_path()?.parent()?;
        Some(StorePath::from_unix_path(parent.as_os_str()))
    }

    /// Runs `then` with the folder identity of `location` and of its parent:
    /// now when the store already answered for both, or once it has, on a
    /// background executor. A location already being resolved is not asked
    /// again; `then` waits for that answer.
    pub(super) fn with_folder_identities(
        &mut self,
        location: StorePath,
        cx: &mut Context<Self>,
        then: impl FnOnce(
            &mut Self,
            StorePath,
            Option<FolderIdentity>,
            Option<FolderIdentity>,
            &mut Context<Self>,
        ) + 'static,
    ) {
        let parent = Self::parent_location(&location);
        let paths = std::iter::once(location.clone())
            .chain(parent.clone())
            .collect::<Vec<_>>();
        let run: FolderIdentityWaiter =
            Box::new(move |state: &mut Self, cx: &mut Context<Self>| {
                let identity = state.folder_identity(&location);
                let parent_identity = parent.as_ref().and_then(|path| state.folder_identity(path));
                then(state, location, identity, parent_identity, cx);
            });
        self.request_folder_identities(paths.clone(), cx);
        if paths
            .iter()
            .any(|path| self.folder_identities_in_flight.contains(path))
        {
            self.folder_identity_waiters.push((paths, run));
        } else {
            run(self, cx);
        }
    }

    /// Runs the waiters whose locations the store has answered for.
    fn run_folder_identity_waiters(&mut self, cx: &mut Context<Self>) {
        for (paths, run) in std::mem::take(&mut self.folder_identity_waiters) {
            if paths
                .iter()
                .any(|path| self.folder_identities_in_flight.contains(path))
            {
                self.folder_identity_waiters.push((paths, run));
            } else {
                run(self, cx);
            }
        }
    }

    /// Asks the store, in the background, for the identities of the
    /// locations it has not answered for, and repaints when it has.
    pub(super) fn request_folder_identities(
        &mut self,
        locations: impl IntoIterator<Item = StorePath>,
        cx: &mut Context<Self>,
    ) {
        let missing = locations
            .into_iter()
            .filter(|path| {
                !self.folder_identities.contains_key(path)
                    && !self.folder_identities_in_flight.contains(path)
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return;
        }
        self.folder_identities_in_flight
            .extend(missing.iter().cloned());
        let store = Arc::clone(&self.store);
        let work = cx.background_spawn(async move { resolve_folder_identities(&*store, missing) });
        cx.spawn(async move |this, cx| {
            let resolved = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                for (path, _) in &resolved {
                    state.folder_identities_in_flight.remove(path);
                }
                state.remember_folder_identities(resolved);
                state.run_folder_identity_waiters(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps the store's answers. A tab whose folder now has another
    /// identity than the one its view preferences came from takes that
    /// folder's catalog preferences.
    pub(super) fn remember_folder_identities(
        &mut self,
        resolved: Vec<(StorePath, Option<FolderIdentity>)>,
    ) {
        let before = resolved
            .iter()
            .map(|(path, _)| (path.clone(), self.folder_identity(path)))
            .collect::<HashMap<_, _>>();
        if self.folder_identities.len() + resolved.len() > FOLDER_IDENTITIES_LIMIT {
            self.folder_identities.clear();
        }
        self.folder_identities.extend(resolved);
        let changed = before
            .into_iter()
            .filter(|(path, identity)| self.folder_identity(path) != *identity)
            .map(|(path, _)| path)
            .collect::<std::collections::HashSet<_>>();
        if changed.is_empty() {
            return;
        }
        let document = self.catalog_binding.snapshot();
        self.apply_catalog_view_preferences(&document, Some(&changed));
    }

    /// Records a visit to `location` in the catalog: its remembered location
    /// and, with a label, a recent location. The identities are resolved
    /// and the catalog is written off the UI thread. Recents keep
    /// navigation order: a recent whose folder answers after a later
    /// navigation's goes below it, and one made before a clear is left out.
    pub(super) fn remember_folder_location(
        &mut self,
        location: StorePath,
        recent_label: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let sequence = recent_label
            .is_some()
            .then(|| self.catalog_binding.next_recent_sequence());
        self.with_folder_identities(
            location,
            cx,
            move |state, location, identity, parent, cx| {
                let Some(identity) = identity else {
                    return;
                };
                let recent = recent_label.and_then(|label| {
                    let newer = state.catalog_binding.claim_recent(sequence?, &identity)?;
                    Some((label, newer))
                });
                let binding = state.catalog_binding.clone();
                state.queue_catalog_work(
                    cx,
                    move || {
                        binding.update(|document| {
                            if let Some((label, newer)) = recent {
                                document.recents_mut().record_before(
                                    &newer,
                                    identity.clone(),
                                    location.clone(),
                                    label,
                                );
                            }
                            document
                                .folder_preferences_mut()
                                .remember_location(identity, location, parent);
                        })
                    },
                    MusheenApp::report_catalog_error,
                );
            },
        );
    }

    pub(super) fn preferences_with_catalog(
        &self,
        location: &StorePath,
        base: crate::views::ViewPreferences,
        catalog: &CatalogDocument,
    ) -> crate::views::ViewPreferences {
        // A view change still on its way to the catalog wins over it.
        if self.pending_view_preferences.contains_key(location) {
            return base;
        }
        let Some(identity) = self.folder_identity(location) else {
            return base;
        };
        let mut reconciled = ViewPreferenceStore::new(base.clone());
        reconciled.set(location.clone(), base);
        reconciled.apply_catalog(&identity, location.clone(), catalog.folder_preferences());
        reconciled.for_path(location).clone()
    }

    pub(super) fn catalog_models(
        session_binding: Option<&SessionBinding>,
    ) -> (CatalogBinding, PinStore, Vec<Box<str>>) {
        let binding = session_binding
            .map(|session| session.catalog.clone())
            .unwrap_or_else(CatalogBinding::in_memory);
        let snapshot = binding.snapshot();
        let pins = PinStore::default();
        pins.replace_catalog(snapshot.pins());
        let tags = binding.tag_names().into_iter().collect();
        (binding, pins, tags)
    }

    pub(super) fn apply_tag_filter(&mut self, tag: &str, cx: &mut Context<Self>) {
        let expression = format!("tag:{tag}");
        self.omnibar.enter(OmnibarMode::Filter, expression.clone());
        self.pending_omnibar_value = Some(expression.clone());
        self.apply_filter(expression, cx);
    }
}

struct TagRenameWindow {
    app: WeakEntity<MusheenApp>,
    old: Box<str>,
    catalog: Catalog,
    input: Entity<InputState>,
    error: Option<Box<str>>,
}

impl TagRenameWindow {
    fn new(
        app: WeakEntity<MusheenApp>,
        old: Box<str>,
        catalog: Catalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = catalog
            .message("catalog-tag-name")
            .expect("the tag-name catalog message exists")
            .to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(old.as_ref())
                .placeholder(placeholder)
        });
        Self {
            app,
            old,
            catalog,
            input,
            error: None,
        }
    }
}

impl Render for TagRenameWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self
            .error
            .as_deref()
            .map(|error| self.catalog.localize_reason(error));
        let rename_tag = self
            .catalog
            .message("catalog-rename-tag")
            .expect("the rename-tag catalog message exists");
        let rename = self
            .catalog
            .message("catalog-rename")
            .expect("the rename catalog message exists");
        div()
            .id("tag-rename-dialog")
            .test_support()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .child(format!("{rename_tag}: ‘{}’", self.old)),
            )
            .child(Input::new(&self.input).id("tag-rename-input"))
            .children(error.map(|error| {
                div()
                    .id("tag-rename-error")
                    .test_support()
                    .role(Role::Alert)
                    .child(error)
            }))
            .child(
                Button::new("tag-rename-confirm")
                    .label(rename)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let new = this.input.read(cx).value().to_string();
                        let old = this.old.clone();
                        let handle = window.window_handle();
                        let dialog = cx.entity().downgrade();
                        let queued =
                            this.app.update(cx, |app, cx| {
                                app.rename_catalog_tag(&old, &new, cx, move |result, cx| {
                                    match result {
                                        Ok(()) => {
                                            let _ = handle
                                                .update(cx, |_, window, _| window.remove_window());
                                        }
                                        Err(error) => {
                                            let _ = dialog.update(cx, |this, cx| {
                                                this.error = Some(error);
                                                cx.notify();
                                            });
                                        }
                                    }
                                });
                            });
                        if let Err(error) = queued {
                            this.error = Some(error.to_string().into());
                        }
                        cx.notify();
                    })),
            )
    }
}

impl MusheenApp {
    pub(super) fn render_home_surface(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mount_locations = self
            .sidebars
            .get(&tab_id)
            .into_iter()
            .flat_map(SidebarModel::sections)
            .find(|section| section.kind() == SidebarSectionKind::Mounts)
            .into_iter()
            .flat_map(|section| {
                section
                    .items()
                    .iter()
                    .map(|entry| entry.location().clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        self.request_folder_identities(mount_locations, cx);
        let mounts = self
            .sidebars
            .get(&tab_id)
            .into_iter()
            .flat_map(SidebarModel::sections)
            .find(|section| section.kind() == SidebarSectionKind::Mounts)
            .into_iter()
            .flat_map(|section| section.items().to_vec())
            .filter_map(|entry| {
                self.folder_identity(entry.location()).map(|identity| {
                    MountShortcut::new(identity, entry.location().clone(), entry.label())
                })
            })
            .collect::<Vec<_>>();
        let sections = self.catalog_binding.home_sections(&mounts);
        let orphaned = self.catalog_binding.orphaned_tags();
        let rows = sections
            .into_iter()
            .map(|section| self.render_home_section(section, cx))
            .collect::<Vec<_>>();
        let orphan_review = self.render_orphan_review(orphaned, cx);
        let home = self
            .catalog
            .message("catalog-home")
            .expect("the Home catalog message exists");
        div()
            .id("home-surface")
            .test_support()
            .role(Role::Main)
            .aria_label(home)
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .children(rows)
            .when_some(orphan_review, |home, review| home.child(review))
            .into_any_element()
    }

    fn render_home_section(&self, section: HomeSection, cx: &mut Context<Self>) -> AnyElement {
        let kind = section.kind();
        let (kind_id, heading_key) = match kind {
            HomeItemKind::Recent => ("recent", "catalog-recent-locations"),
            HomeItemKind::Pin => ("pin", "catalog-pinned"),
            HomeItemKind::Mount => ("mount", "catalog-storage"),
            HomeItemKind::Tag => ("tag", "catalog-tags"),
        };
        let heading = self
            .catalog
            .message(heading_key)
            .expect("the Home section catalog message exists")
            .to_owned();
        let unpin = self
            .catalog
            .message("catalog-unpin")
            .expect("the unpin catalog message exists")
            .to_owned();
        let catalog = self.catalog.clone();
        let rtl = self.catalog.locale() == Locale::Ar;
        let items = section.items().iter().enumerate().map(|(index, item)| {
            let path = item.path_hint().cloned();
            let pin_path = path.clone();
            let pin_identity = item.identity().cloned();
            let tag = (kind == HomeItemKind::Tag).then(|| item.label().to_owned());
            let unavailable = item.unavailable_reason().map(str::to_owned);
            let localized_unavailable = unavailable
                .as_deref()
                .map(|reason| catalog.localize_reason(reason));
            let accessibility_label = unavailable.as_deref().map_or_else(
                || item.label().to_owned(),
                |reason| catalog.unavailable_label(item.label(), reason),
            );
            div()
                .w_full()
                .flex()
                .items_center()
                .when(rtl, |row| row.flex_row_reverse())
                .child(
                    Button::new(SharedString::from(format!("home-item-{kind_id}-{index}")))
                        .label(item.label().to_owned())
                        .accessibility_label(accessibility_label)
                        .ghost()
                        .small()
                        .disabled(localized_unavailable.is_some())
                        .when_some(localized_unavailable, |button, reason| {
                            button.tooltip(reason)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(tag) = tag.as_deref() {
                                this.apply_tag_filter(tag, cx);
                            } else if let Some(path) = &path {
                                this.navigate(path.clone(), true, cx);
                            }
                        })),
                )
                .when(
                    kind == HomeItemKind::Pin && pin_identity.is_some() && pin_path.is_some(),
                    |row| {
                        row.child(
                            Button::new(SharedString::from(format!("home-unpin-{index}")))
                                .label(unpin.clone())
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let (Some(item), Some(path)) =
                                        (pin_identity.clone(), pin_path.clone())
                                    else {
                                        return;
                                    };
                                    this.unpin_home_item(item, path, cx);
                                })),
                        )
                    },
                )
        });
        div()
            .id(SharedString::from(format!("home-section-{kind_id}")))
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(heading))
            .children(items)
            .into_any_element()
    }

    fn render_orphan_review(
        &self,
        orphaned: Vec<OrphanedTagRecord>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if orphaned.is_empty() {
            return None;
        }
        let remove = self
            .catalog
            .message("catalog-remove-metadata")
            .expect("the remove-metadata catalog message exists")
            .to_owned();
        let remove_accessibility = self
            .catalog
            .message("catalog-remove-reviewed-orphan")
            .expect("the orphan action catalog message exists")
            .to_owned();
        let review = self
            .catalog
            .message("catalog-orphan-review")
            .expect("the orphan review catalog message exists")
            .to_owned();
        let heading = self
            .catalog
            .message("catalog-orphan-heading")
            .expect("the orphan heading catalog message exists")
            .to_owned();
        let rtl = self.catalog.locale() == Locale::Ar;
        let rows = orphaned.into_iter().enumerate().map(|(index, orphan)| {
            let item = orphan.item.clone();
            let tags = orphan
                .tags
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
                .join(", ");
            div()
                .w_full()
                .flex()
                .items_center()
                .when(rtl, |row| row.flex_row_reverse())
                .gap_2()
                .child(div().flex_grow(1.0).text_sm().child(format!(
                    "{} — {tags}",
                    DisplayPath::from_store_path(&orphan.path).as_str()
                )))
                .child(
                    Button::new(SharedString::from(format!("home-orphan-cleanup-{index}")))
                        .label(remove.clone())
                        .accessibility_label(remove_accessibility.clone())
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cleanup_reviewed_orphan(item.clone(), cx);
                        })),
                )
        });
        Some(
            div()
                .id("home-orphan-review")
                .test_support()
                .role(Role::Region)
                .aria_label(review)
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child(heading))
                .children(rows)
                .into_any_element(),
        )
    }

    fn unpin_home_item(&mut self, item: ItemId, path: StorePath, cx: &mut Context<Self>) {
        let Ok(target) = CommandTargetRef::new(item, path.clone()) else {
            return;
        };
        let menu = self.compose_context_menu_at(
            self.navigation.focused_tab().id(),
            MenuTarget::SidebarLocation,
            path,
            vec![target],
        );
        if let Some(entry) = menu.entry("directory.unpin").cloned() {
            self.dispatch_context_entry(entry, cx);
        }
    }

    /// Removes a reviewed orphan's tags unless the store finds the item
    /// again. The store call runs off the UI thread, then the catalog write
    /// is queued. A second request while one runs is ignored.
    pub(super) fn cleanup_reviewed_orphan(&mut self, item: ItemId, cx: &mut Context<Self>) {
        if !self.orphan_cleanups.insert(item.clone()) {
            return;
        }
        let store = Arc::clone(&self.store);
        let binding = self.catalog_binding.clone();
        let executor = cx.background_executor().clone();
        let reviewed = item.clone();
        let probe = cx.background_spawn(async move {
            let path = match binding.orphan_path(&reviewed) {
                Ok(Some(path)) => path,
                outcome => {
                    let (sender, receiver) = async_channel::bounded(1);
                    let _ =
                        sender.try_send(outcome.map(|_| OrphanCleanupOutcome::NoLongerOrphaned));
                    return receiver;
                }
            };
            let found = store
                .resolve_item(&path)
                .map_err(|error| Box::<str>::from(error.to_string()));
            let writer = binding.clone();
            binding.queue_work(&executor, move || {
                writer.finish_orphan_cleanup(&reviewed, &path, found)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = catalog_work_result(probe.await).await;
            let _ = this.update(cx, |state, cx| {
                state.orphan_cleanups.remove(&item);
                match result {
                    Ok(OrphanCleanupOutcome::Removed | OrphanCleanupOutcome::StillPresent) => {
                        state.sync_catalog_projection(cx);
                    }
                    Ok(OrphanCleanupOutcome::NoLongerOrphaned) => {
                        state.operation_error = Some(
                            state
                                .catalog
                                .message("catalog-error-orphan-live")
                                .expect("the orphan-live catalog message exists")
                                .into(),
                        );
                    }
                    Err(error) => state.operation_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// The result of queued catalog work: an error when the work stopped
/// without one, as a job that panicked does.
pub(super) async fn catalog_work_result<T>(
    written: async_channel::Receiver<Result<T, Box<str>>>,
) -> Result<T, Box<str>> {
    written.recv().await.unwrap_or_else(|_| {
        Err(Box::<str>::from(
            "the catalog write stopped before it finished",
        ))
    })
}

/// Resolves folder identities at the store. Runs on a background executor.
fn resolve_folder_identities(
    store: &dyn Store,
    locations: Vec<StorePath>,
) -> Vec<(StorePath, Option<FolderIdentity>)> {
    locations
        .into_iter()
        .map(|location| {
            let identity = store
                .resolve_item(&location)
                .ok()
                .flatten()
                .map(|item| FolderIdentity::from_item(item.id().clone()));
            (location, identity)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{CatalogBinding, CatalogWriteQueue, DirectoryObservation, OrphanCleanupOutcome};
    use musheen_core::{
        CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, ItemId, ProviderId,
        StorePath,
    };
    use musheen_core::{DisplayPath, ItemKind, StoreItem};
    use musheen_desktop::{TagMoveOutcome, TagStorage};
    use standard_library::fs as filesystem;
    use std as standard_library;
    use std::collections::BTreeSet;

    fn provider(name: &str) -> ProviderId {
        ProviderId::new(name).expect("valid provider")
    }

    fn item(provider_name: &str, key: &[u8]) -> ItemId {
        ItemId::new(provider(provider_name), key.to_vec()).expect("valid item identity")
    }

    fn remote_path(provider_name: &str, key: &[u8]) -> StorePath {
        StorePath::from_provider_key(provider(provider_name), key.to_vec()).expect("valid path")
    }

    fn capabilities(tags: bool, xattrs: bool) -> CapabilityMatrix {
        CapabilityMatrix::new(|kind| {
            let supported = match kind {
                CapabilityKind::Tags => tags,
                CapabilityKind::ExtendedAttributes => xattrs,
                _ => true,
            };
            if supported {
                CapabilityState::Supported
            } else {
                CapabilityState::Unsupported(
                    CapabilityReason::new("not supported by this provider").unwrap(),
                )
            }
        })
    }

    #[test]
    fn binding_routes_tag_writes_through_the_production_service() {
        let binding = CatalogBinding::in_memory_with_xattr_opt_in(false);
        let target = item("remote", b"stable-item");
        let path = remote_path("remote", b"folder/file");

        assert_eq!(
            binding
                .assign_tag(&target, &path, &capabilities(true, false), "blue")
                .unwrap(),
            TagStorage::AppCatalog
        );
        assert_eq!(
            binding
                .tags_for(&target, &path, &capabilities(true, false))
                .unwrap(),
            [Box::<str>::from("blue")].into_iter().collect()
        );
    }

    #[test]
    fn read_only_catalog_queries_do_not_advance_the_projection_revision() {
        let binding = CatalogBinding::in_memory();
        let target = item("remote", b"stable-item");
        let path = remote_path("remote", b"folder/file");
        let revision = binding.revision();

        assert!(
            binding
                .tags_for(&target, &path, &capabilities(true, false))
                .unwrap()
                .is_empty()
        );
        assert_eq!(binding.revision(), revision);
    }

    #[test]
    fn multi_selection_tag_additions_preserve_item_specific_tags() {
        let binding = CatalogBinding::in_memory();
        let first = item("remote", b"first");
        let second = item("remote", b"second");
        let first_path = remote_path("remote", b"folder/first");
        let second_path = remote_path("remote", b"folder/second");
        let matrix = capabilities(true, false);
        binding
            .assign_tag(&first, &first_path, &matrix, "red")
            .unwrap();
        binding
            .assign_tag(&first, &first_path, &matrix, "private")
            .unwrap();
        binding
            .assign_tag(&second, &second_path, &matrix, "red")
            .unwrap();

        binding
            .apply_tag_delta(
                &[
                    (first.clone(), first_path, matrix.clone()),
                    (second.clone(), second_path, matrix),
                ],
                &[Box::<str>::from("blue")].into_iter().collect(),
                &BTreeSet::new(),
            )
            .unwrap();

        assert_eq!(
            binding.tags_for_identity(&first),
            ["blue", "private", "red"]
                .into_iter()
                .map(Box::<str>::from)
                .collect()
        );
        assert_eq!(
            binding.tags_for_identity(&second),
            ["blue", "red"].into_iter().map(Box::<str>::from).collect()
        );
    }

    #[test]
    fn failed_tag_mirror_batch_persists_desired_state_for_deterministic_recovery() {
        let temporary = tempfile::tempdir().unwrap();
        let catalog_store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        let first_path = temporary.path().join("first");
        let second_path = temporary.path().join("second");
        filesystem::write(&first_path, b"first").unwrap();
        filesystem::write(&second_path, b"second").unwrap();
        let first = item("local", b"first");
        let second = item("local", b"second");
        let first_path = StorePath::from_unix_path(first_path.as_os_str());
        let second_path = StorePath::from_unix_path(second_path.as_os_str());
        let mut document = musheen_desktop::CatalogDocument::default();
        document
            .tags_mut()
            .assign(&first, first_path.clone(), "red")
            .unwrap();
        document
            .tags_mut()
            .assign(&second, second_path.clone(), "red")
            .unwrap();
        catalog_store.save(&document).unwrap();
        let binding =
            CatalogBinding::persistent_with_xattr_opt_in(catalog_store.clone(), document, true);
        let matrix = capabilities(true, true);
        let targets = [
            (first.clone(), first_path, matrix.clone()),
            (second.clone(), second_path, matrix),
        ];
        let mut writes = 0;

        let error = binding
            .apply_tag_delta_with_xattr_writer(
                &targets,
                &[Box::<str>::from("blue")].into_iter().collect(),
                &BTreeSet::new(),
                |_, _, _| {
                    writes += 1;
                    if writes == 2 {
                        Err(Box::<str>::from("injected second mirror failure"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert_eq!(error.as_ref(), "injected second mirror failure");
        let durable = catalog_store.load().unwrap();
        assert_eq!(durable.tags().pending_xattr_count(), 1);
        assert!(!durable.tags().is_xattr_pending(&first));
        assert!(durable.tags().is_xattr_pending(&second));
        assert_eq!(
            durable.tags().tags_for(&first),
            ["blue", "red"].into_iter().map(Box::<str>::from).collect()
        );
        assert_eq!(
            durable.tags().tags_for(&second),
            ["blue", "red"].into_iter().map(Box::<str>::from).collect()
        );

        binding
            .reconcile_pending_xattrs_with(|_, _, _| Ok(()))
            .unwrap();
        assert_eq!(
            catalog_store.load().unwrap().tags().pending_xattr_count(),
            0
        );
    }

    #[test]
    fn global_tag_changes_include_catalog_only_orphans_without_touching_missing_xattrs() {
        let temporary = tempfile::tempdir().unwrap();
        let active_path = temporary.path().join("active");
        filesystem::write(&active_path, b"active").unwrap();
        let active = item("local", b"active");
        let orphan = item("local", b"orphan");
        let active_path = StorePath::from_unix_path(active_path.into_os_string());
        let orphan_path = StorePath::from_unix_path(temporary.path().join("missing"));
        let mut document = musheen_desktop::CatalogDocument::default();
        document
            .tags_mut()
            .assign(&active, active_path.clone(), "old")
            .unwrap();
        document
            .tags_mut()
            .assign(&orphan, orphan_path, "old")
            .unwrap();
        document.tags_mut().observe_missing(&orphan);
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        store.save(&document).unwrap();
        let binding = CatalogBinding::persistent_with_xattr_opt_in(store, document, true);

        assert_eq!(
            binding
                .rename_tag("old", "new", |_| capabilities(true, true))
                .unwrap(),
            2
        );
        assert_eq!(
            binding.tags_for_identity(&orphan),
            [Box::<str>::from("new")].into_iter().collect()
        );
        assert!(binding.is_orphaned(&orphan));
        assert_eq!(
            binding
                .delete_tag("new", |_| capabilities(true, true))
                .unwrap(),
            2
        );
        assert!(binding.tags_for_identity(&orphan).is_empty());
    }

    #[test]
    fn independent_catalog_bindings_reject_a_stale_write_without_losing_updates() {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        let document = musheen_desktop::CatalogDocument::default();
        store.save(&document).unwrap();
        let first_binding = CatalogBinding::persistent(store.clone(), document.clone());
        let second_binding = CatalogBinding::persistent(store.clone(), document);
        let initial_revision = first_binding.revision();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let stale_binding = first_binding.clone();
        let first = std::thread::spawn(move || {
            stale_binding.update(|document| {
                document
                    .pins_mut()
                    .pin(
                        item("local", b"first-binding"),
                        StorePath::from_unix_path("/first"),
                        "First",
                    )
                    .unwrap();
                entered_tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(100));
            })
        });
        entered_rx.recv().unwrap();
        let second = std::thread::spawn(move || {
            second_binding.update(|document| {
                document
                    .pins_mut()
                    .pin(
                        item("local", b"second-binding"),
                        StorePath::from_unix_path("/second"),
                        "Second",
                    )
                    .unwrap();
            })
        });
        assert_eq!(
            first.join().unwrap().unwrap_err().as_ref(),
            "catalog changed before the update could be committed safely; retry the action"
        );
        second.join().unwrap().unwrap();
        assert!(first_binding.revision() > initial_revision);
        let winner = first_binding.snapshot();
        assert_eq!(winner.pins().entries().len(), 1);
        assert_eq!(
            winner.pins().entries()[0].item().opaque_key(),
            b"second-binding"
        );

        first_binding
            .update(|document| {
                document
                    .pins_mut()
                    .pin(
                        item("local", b"first-binding"),
                        StorePath::from_unix_path("/first"),
                        "First",
                    )
                    .unwrap();
            })
            .unwrap();

        assert_eq!(store.load().unwrap().pins().entries().len(), 2);
    }

    #[test]
    fn independent_disk_changes_advance_a_binding_revision_on_refresh() {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        store
            .save(&musheen_desktop::CatalogDocument::default())
            .unwrap();
        let first =
            CatalogBinding::persistent(store.clone(), musheen_desktop::CatalogDocument::default());
        let second = CatalogBinding::persistent(store, musheen_desktop::CatalogDocument::default());
        second
            .update(|document| {
                document
                    .pins_mut()
                    .pin(
                        item("local", b"external"),
                        StorePath::from_unix_path("/external"),
                        "External",
                    )
                    .unwrap();
            })
            .unwrap();
        let revision = first.revision();

        first.update(|_| {}).unwrap();

        assert!(first.revision() > revision);
        assert_eq!(first.snapshot().pins().entries().len(), 1);
    }

    #[test]
    fn stale_xattr_worker_rewrites_the_latest_desired_tags_before_clearing() {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        let target = item("local", b"raced-item");
        let path = StorePath::from_unix_path(temporary.path().join("raced-item"));
        filesystem::write(path.as_unix_path().unwrap(), b"fixture").unwrap();
        let mut document = musheen_desktop::CatalogDocument::default();
        document.tags_mut().stage_xattr_tags(
            &target,
            path.clone(),
            [Box::<str>::from("old")].into_iter().collect(),
        );
        store.save(&document).unwrap();
        let binding = CatalogBinding::persistent_with_xattr_opt_in(store, document, true);
        let worker = binding.clone();
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker_writes = std::sync::Arc::clone(&writes);
        let (old_written_tx, old_written_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let stale = std::thread::spawn(move || {
            let mut first = true;
            worker.reconcile_pending_xattrs_with(|_, _, tags| {
                worker_writes.lock().unwrap().push(tags.clone());
                if first {
                    first = false;
                    old_written_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                Ok(())
            })
        });
        old_written_rx.recv().unwrap();
        binding
            .apply_tag_delta_with_xattr_writer(
                &[(target.clone(), path.clone(), capabilities(true, true))],
                &[Box::<str>::from("new")].into_iter().collect(),
                &[Box::<str>::from("old")].into_iter().collect(),
                |_, _, _| Ok(()),
            )
            .unwrap();
        release_tx.send(()).unwrap();
        stale.join().unwrap().unwrap();

        assert_eq!(
            writes.lock().unwrap().last().cloned().unwrap(),
            [Box::<str>::from("new")].into_iter().collect()
        );
        assert!(!binding.snapshot().tags().is_xattr_pending(&target));
    }

    #[test]
    fn stale_xattr_import_never_writes_after_a_concurrent_delete_wins_cas() {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        let target = item("local", b"stale-import");
        let path = StorePath::from_unix_path(temporary.path().join("stale-import"));
        filesystem::write(path.as_unix_path().unwrap(), b"fixture").unwrap();
        let mut document = musheen_desktop::CatalogDocument::default();
        document
            .tags_mut()
            .assign(&target, path.clone(), "catalog-only")
            .unwrap();
        store.save(&document).unwrap();
        let stale_binding =
            CatalogBinding::persistent_with_xattr_opt_in(store.clone(), document.clone(), true);
        let stale_observer = stale_binding.clone();
        let delete_binding =
            CatalogBinding::persistent_with_xattr_opt_in(store.clone(), document, true);
        let stale_target = target.clone();
        let stale_path = path.clone();
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stale_writes = std::sync::Arc::clone(&writes);
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let stale = std::thread::spawn(move || {
            stale_binding.tags_for_with_xattr_io(
                &stale_target,
                &stale_path,
                &capabilities(true, true),
                |_, _| {
                    read_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok([Box::<str>::from("deleted")].into_iter().collect())
                },
                |_, _, tags| {
                    stale_writes.lock().unwrap().push(tags.clone());
                    Ok(())
                },
            )
        });
        read_rx.recv().unwrap();
        delete_binding
            .update(|document| {
                document.tags_mut().remove(&target, "catalog-only");
            })
            .unwrap();
        release_tx.send(()).unwrap();

        assert!(stale.join().unwrap().is_err());
        assert!(writes.lock().unwrap().is_empty());
        assert!(
            stale_observer
                .snapshot()
                .tags()
                .tags_for(&target)
                .is_empty()
        );
        let durable = store.load().unwrap();
        assert!(durable.tags().tags_for(&target).is_empty());
        assert!(!durable.tags().is_xattr_pending(&target));
    }

    #[test]
    fn interrupted_durable_delete_intent_replays_empty_xattrs_after_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        let target = item("local", b"restart-delete");
        let path = StorePath::from_unix_path(temporary.path().join("restart-delete"));
        filesystem::write(path.as_unix_path().unwrap(), b"fixture").unwrap();
        let mut document = musheen_desktop::CatalogDocument::default();
        document
            .tags_mut()
            .assign(&target, path.clone(), "deleted")
            .unwrap();
        store.save(&document).unwrap();
        let binding = CatalogBinding::persistent_with_xattr_opt_in(store.clone(), document, true);

        let error = binding
            .apply_tag_delta_with_xattr_writer(
                &[(target.clone(), path, capabilities(true, true))],
                &BTreeSet::new(),
                &[Box::<str>::from("deleted")].into_iter().collect(),
                |_, _, _| {
                    Err(Box::<str>::from(
                        "simulated interruption before xattr write",
                    ))
                },
            )
            .unwrap_err();
        assert_eq!(error.as_ref(), "simulated interruption before xattr write");
        let pending = store.load().unwrap();
        assert!(pending.tags().tags_for(&target).is_empty());
        assert!(pending.tags().is_xattr_pending(&target));

        let restarted = CatalogBinding::persistent_with_xattr_opt_in(store.clone(), pending, true);
        let mut replayed = Vec::new();
        restarted
            .reconcile_pending_xattrs_with(|_, _, tags| {
                replayed.push(tags.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(replayed, vec![BTreeSet::new()]);
        let durable = store.load().unwrap();
        assert!(durable.tags().tags_for(&target).is_empty());
        assert!(!durable.tags().is_xattr_pending(&target));
    }

    #[test]
    fn interrupted_xattr_import_persists_intent_before_write_and_replays_after_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            musheen_desktop::CatalogStore::at(temporary.path().join("private/catalog.json"));
        let target = item("local", b"restart-import");
        let path = StorePath::from_unix_path(temporary.path().join("restart-import"));
        filesystem::write(path.as_unix_path().unwrap(), b"fixture").unwrap();
        let mut document = musheen_desktop::CatalogDocument::default();
        document
            .tags_mut()
            .assign(&target, path.clone(), "catalog-only")
            .unwrap();
        store.save(&document).unwrap();
        let binding = CatalogBinding::persistent_with_xattr_opt_in(store.clone(), document, true);
        let expected = ["catalog-only", "disk-only"]
            .into_iter()
            .map(Box::<str>::from)
            .collect::<BTreeSet<_>>();
        let inspection_store = store.clone();

        let error = binding
            .tags_for_with_xattr_io(
                &target,
                &path,
                &capabilities(true, true),
                |_, _| Ok([Box::<str>::from("disk-only")].into_iter().collect()),
                |_, _, tags| {
                    let durable = inspection_store.load().unwrap();
                    assert!(durable.tags().is_xattr_pending(&target));
                    assert_eq!(durable.tags().tags_for(&target), expected);
                    assert_eq!(tags, &expected);
                    Err(Box::<str>::from(
                        "simulated interruption before xattr write",
                    ))
                },
            )
            .unwrap_err();
        assert_eq!(error.as_ref(), "simulated interruption before xattr write");

        let pending = store.load().unwrap();
        let restarted = CatalogBinding::persistent_with_xattr_opt_in(store.clone(), pending, true);
        let mut replayed = Vec::new();
        restarted
            .reconcile_pending_xattrs_with(|_, _, tags| {
                replayed.push(tags.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(replayed, vec![expected.clone()]);
        let durable = store.load().unwrap();
        assert_eq!(durable.tags().tags_for(&target), expected);
        assert!(!durable.tags().is_xattr_pending(&target));
    }

    #[test]
    fn ordinary_large_listings_do_not_persist_empty_tag_records() {
        let binding = CatalogBinding::in_memory();
        let directory = remote_path("remote", b"large-scope");
        let items = (0..4_097)
            .map(|index| {
                let key = format!("child-{index}");
                StoreItem::new(
                    item("remote", key.as_bytes()),
                    remote_path("remote", key.as_bytes()),
                    DisplayPath::new(key),
                    ItemKind::RegularFile,
                    None,
                )
            })
            .collect::<Vec<_>>();
        let tagged = items.last().unwrap().clone();

        binding
            .reconcile_directory(&directory, &items, DirectoryObservation::Complete)
            .unwrap();
        assert_eq!(binding.snapshot().tags().tracked_items().count(), 0);

        binding
            .assign_tag(
                tagged.id(),
                tagged.path(),
                &capabilities(true, false),
                "tracked",
            )
            .unwrap();
        assert_eq!(
            binding
                .snapshot()
                .tags()
                .tracked_scoped_items()
                .find(|(item, _, _, _)| *item == tagged.id())
                .and_then(|(_, _, scope, _)| scope),
            Some(&directory)
        );

        binding
            .reconcile_directory(&directory, &[], DirectoryObservation::Complete)
            .unwrap();
        assert!(binding.is_orphaned(tagged.id()));
    }

    #[test]
    fn binding_preserves_provider_paths_and_refuses_unsupported_move_targets() {
        let binding = CatalogBinding::in_memory();
        let source = item("local", b"inode-1");
        let source_path = StorePath::from_unix_bytes(b"/source/file".to_vec());
        binding
            .assign_tag(&source, &source_path, &capabilities(true, false), "keep")
            .unwrap();

        let remote = item("remote", b"object-2");
        let destination_path = remote_path("remote", b"share/final");
        assert_eq!(
            binding
                .complete_move(
                    &source,
                    remote.clone(),
                    destination_path.clone(),
                    &capabilities(true, false),
                )
                .unwrap(),
            TagMoveOutcome::Preserved
        );
        assert_eq!(binding.path_hint(&remote), Some(destination_path));

        let archive = item("archive", b"entry-3");
        let archive_path = remote_path("archive", b"entry-3");
        assert_eq!(
            binding
                .complete_move(
                    &remote,
                    archive.clone(),
                    archive_path,
                    &capabilities(false, false),
                )
                .unwrap(),
            TagMoveOutcome::UnsupportedDestination
        );
        assert_eq!(binding.tags_for_identity(&remote).len(), 1);
        assert!(binding.tags_for_identity(&archive).is_empty());
        assert!(binding.is_orphaned(&remote));
        assert!(!binding.items_with_tag("keep").contains(&remote));
        assert!(
            binding
                .orphaned_tags()
                .iter()
                .any(|record| record.item == remote)
        );
    }

    #[test]
    fn binding_reconciles_external_changes_by_exact_identity() {
        let binding = CatalogBinding::in_memory();
        let target = item("local", b"inode-9");
        let old_path = StorePath::from_unix_bytes(b"/disk/old".to_vec());
        let renamed_path = StorePath::from_unix_bytes(b"/disk/new".to_vec());
        binding
            .assign_tag(&target, &old_path, &capabilities(true, false), "tracked")
            .unwrap();

        binding
            .observe_present(&target, renamed_path.clone())
            .unwrap();
        assert_eq!(binding.path_hint(&target), Some(renamed_path));
        binding.observe_missing(&target).unwrap();
        assert!(binding.is_orphaned(&target));

        let reused_path = item("local", b"inode-10");
        assert_eq!(binding.cleanup_reviewed_orphans([&reused_path]).unwrap(), 0);
        assert_eq!(binding.cleanup_reviewed_orphans([&target]).unwrap(), 1);
    }

    #[test]
    fn binding_reconciles_each_pin_without_blocking_siblings() {
        let binding = CatalogBinding::in_memory();
        let online = item("remote", b"online");
        let offline = item("remote", b"offline");
        let online_path = remote_path("remote", b"share/online");
        let offline_path = remote_path("remote", b"share/offline");
        binding
            .update(|document| {
                document
                    .pins_mut()
                    .pin(online.clone(), online_path.clone(), "Online")
                    .unwrap();
                document
                    .pins_mut()
                    .pin(offline.clone(), offline_path.clone(), "Offline")
                    .unwrap();
            })
            .unwrap();

        binding
            .reconcile_pins(|path| {
                if path == &online_path {
                    Ok(Some(StoreItem::new(
                        online.clone(),
                        online_path.clone(),
                        DisplayPath::new("Online"),
                        ItemKind::Directory,
                        None,
                    )))
                } else {
                    Err(Box::<str>::from("remote account offline"))
                }
            })
            .unwrap();

        let snapshot = binding.snapshot();
        assert!(matches!(
            snapshot.pins().entries()[0].state(),
            musheen_desktop::PinState::Available
        ));
        assert!(matches!(
            snapshot.pins().entries()[1].state(),
            musheen_desktop::PinState::Unavailable(reason)
                if reason.as_ref() == "remote account offline"
        ));
    }

    #[test]
    fn slow_provider_resolution_does_not_block_an_independent_catalog_update() {
        let pinned = item("remote", b"slow-pin");
        let pinned_path = remote_path("remote", b"slow-pin");
        let binding = CatalogBinding::in_memory();
        binding
            .update(|document| {
                document
                    .pins_mut()
                    .pin(pinned, pinned_path, "Slow")
                    .unwrap();
            })
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let resolving = binding.clone();
        let worker = std::thread::spawn(move || {
            resolving.reconcile_pins(|_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(None)
            })
        });
        entered_rx.recv().unwrap();
        assert!(
            binding.document.try_lock().is_ok(),
            "provider resolution must not hold the catalog lock"
        );
        binding
            .update(|document| {
                document
                    .pins_mut()
                    .pin(
                        item("local", b"available"),
                        StorePath::from_unix_path("/available"),
                        "Available",
                    )
                    .unwrap();
            })
            .unwrap();
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(binding.snapshot().pins().entries().len(), 2);
    }

    #[test]
    fn directory_reconciliation_requires_authoritative_completion_before_orphaning() {
        let binding = CatalogBinding::in_memory();
        let renamed = item("local", b"inode-41");
        let missing = item("local", b"inode-42");
        let directory = StorePath::from_unix_path("/scope");
        let old_path = StorePath::from_unix_path("/scope/old-name");
        let new_path = StorePath::from_unix_path("/scope/new-name");
        let missing_path = StorePath::from_unix_path("/scope/missing");
        for (item, path) in [(&renamed, &old_path), (&missing, &missing_path)] {
            binding
                .assign_tag(item, path, &capabilities(true, false), "tracked")
                .unwrap();
        }
        let listing = vec![StoreItem::new(
            renamed.clone(),
            new_path.clone(),
            DisplayPath::new("new-name"),
            ItemKind::RegularFile,
            None,
        )];

        binding
            .reconcile_directory(&directory, &listing, DirectoryObservation::Partial)
            .unwrap();

        assert_eq!(binding.path_hint(&renamed), Some(new_path));
        assert!(!binding.is_orphaned(&renamed));
        assert!(!binding.is_orphaned(&missing));

        binding
            .reconcile_directory(&directory, &listing, DirectoryObservation::Complete)
            .unwrap();
        assert!(binding.is_orphaned(&missing));
    }

    #[test]
    fn authoritative_opaque_listing_orphans_disappeared_items_by_observed_scope() {
        let binding = CatalogBinding::in_memory();
        let directory = remote_path("remote", b"opaque-container-token");
        let target = item("remote", b"stable-child");
        let path = remote_path("remote", b"opaque-child-token");
        let listed = StoreItem::new(
            target.clone(),
            path.clone(),
            DisplayPath::new("child"),
            ItemKind::RegularFile,
            None,
        );
        binding
            .reconcile_directory(
                &directory,
                std::slice::from_ref(&listed),
                DirectoryObservation::Complete,
            )
            .unwrap();
        binding
            .assign_tag(&target, &path, &capabilities(true, false), "tracked")
            .unwrap();

        binding
            .reconcile_directory(&directory, &[], DirectoryObservation::Complete)
            .unwrap();

        assert!(binding.is_orphaned(&target));
        assert!(!binding.items_with_tag("tracked").contains(&target));
    }

    #[test]
    fn reviewed_orphan_cleanup_rechecks_exact_identity_liveness() {
        let binding = CatalogBinding::in_memory();
        let target = item("local", b"inode-99");
        let path = StorePath::from_unix_path("/scope/target");
        binding
            .assign_tag(&target, &path, &capabilities(true, false), "tracked")
            .unwrap();
        binding.observe_missing(&target).unwrap();

        assert_eq!(
            binding
                .cleanup_reviewed_orphan(&target, |_| {
                    Ok(Some(StoreItem::new(
                        target.clone(),
                        path.clone(),
                        DisplayPath::new("target"),
                        ItemKind::RegularFile,
                        None,
                    )))
                })
                .unwrap(),
            OrphanCleanupOutcome::StillPresent
        );
        assert!(!binding.is_orphaned(&target));

        binding.observe_missing(&target).unwrap();
        assert_eq!(
            binding
                .cleanup_reviewed_orphan(&target, |_| Ok(None))
                .unwrap(),
            OrphanCleanupOutcome::Removed
        );
        assert!(binding.tags_for_identity(&target).is_empty());
    }

    // UXF-023: the catalog write queue runs jobs in the order they were
    // queued, keeps going after a job that fails, and asks for a new drain
    // once it is empty.
    #[test]
    fn ui_thread_catalog_write_queue_keeps_order_and_survives_a_failed_write() {
        let queue = CatalogWriteQueue::default();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        for number in 0..3 {
            let order = std::sync::Arc::clone(&order);
            let starts_drain = queue.push(Box::new(move || {
                assert_ne!(number, 1, "the second write fails");
                order.lock().unwrap().push(number);
            }));
            assert_eq!(
                starts_drain,
                number == 0,
                "only the first job starts a drain"
            );
        }
        queue.drain();
        assert_eq!(*order.lock().unwrap(), [0, 2]);
        assert!(
            queue.push(Box::new(|| {})),
            "a drained queue asks for a new drain"
        );
    }

    // UXF-023: while one queued write waits, as a write waiting on another
    // process's catalog lock does, writes queued from other threads wait in
    // the queue and hold no thread of their own.
    #[test]
    fn ui_thread_a_waiting_write_keeps_the_queue_on_one_thread() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex, mpsc};
        let queue = CatalogWriteQueue::default();
        let started = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        let (entered, entered_rx) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        {
            let (started, order) = (Arc::clone(&started), Arc::clone(&order));
            assert!(queue.push(Box::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                released.recv().unwrap();
                order.lock().unwrap().push(0);
            })));
        }
        let drain = {
            let queue = queue.clone();
            std::thread::spawn(move || queue.drain())
        };
        entered_rx.recv().unwrap();
        let pushers = (1..=8)
            .map(|number| {
                let (queue, started, order) =
                    (queue.clone(), Arc::clone(&started), Arc::clone(&order));
                std::thread::spawn(move || {
                    queue.push(Box::new(move || {
                        started.fetch_add(1, Ordering::SeqCst);
                        order.lock().unwrap().push(number);
                    }))
                })
            })
            .collect::<Vec<_>>();
        for pusher in pushers {
            assert!(
                !pusher.join().unwrap(),
                "the running drain takes the write; no new thread starts"
            );
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "only the waiting write has started"
        );
        release.send(()).unwrap();
        drain.join().unwrap();
        let order = order.lock().unwrap();
        assert_eq!(order.len(), 9, "every queued write runs");
        assert_eq!(order[0], 0, "the waiting write lands first");
    }

    // UXF-023: the queue settles once the writes queued before the question
    // have run; an idle queue is settled at once. Quitting waits on this.
    #[test]
    fn ui_thread_catalog_write_queue_settles_after_its_last_write() {
        use futures_lite::future::{block_on, poll_once};
        let queue = CatalogWriteQueue::default();
        assert!(
            block_on(poll_once(Box::pin(queue.settled()))).is_some(),
            "an idle queue is settled"
        );
        let (release, released) = std::sync::mpsc::channel::<()>();
        assert!(queue.push(Box::new(move || {
            released.recv().unwrap();
        })));
        let mut settled = Box::pin(queue.settled());
        assert!(
            block_on(poll_once(&mut settled)).is_none(),
            "a queue with a write left is not settled"
        );
        let drain = {
            let queue = queue.clone();
            std::thread::spawn(move || queue.drain())
        };
        release.send(()).unwrap();
        drain.join().unwrap();
        block_on(settled);
    }

    // UXF-023: a tag rename and a tag delete queued while another process
    // holds the catalog lock both apply once it is released, because each
    // reads the catalog inside the queue after the write before it.
    #[test]
    fn ui_thread_queued_tag_rename_and_delete_both_apply_after_the_lock_is_released() {
        let temporary = tempfile::tempdir().unwrap();
        let catalog_path = temporary.path().join("catalog.json");
        let store = musheen_desktop::CatalogStore::at(&catalog_path);
        let tagged = item("local", b"tagged");
        let path = StorePath::from_unix_path("/tagged");
        let mut document = musheen_desktop::CatalogDocument::default();
        document
            .tags_mut()
            .assign(&tagged, path.clone(), "Work")
            .unwrap();
        document.tags_mut().assign(&tagged, path, "Old").unwrap();
        store.save(&document).unwrap();
        let binding = CatalogBinding::persistent(store, document);

        let mut lock_path = catalog_path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock_file = filesystem::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        rustix::fs::flock(&lock_file, rustix::fs::FlockOperation::LockExclusive).unwrap();

        let results = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let queue_change = |change: fn(&CatalogBinding) -> Result<usize, Box<str>>| {
            let binding = binding.clone();
            let results = std::sync::Arc::clone(&results);
            std::thread::spawn(move || {
                let writer = binding.clone();
                let starts_drain = binding.writes.push(Box::new(move || {
                    results.lock().unwrap().push(change(&writer));
                }));
                if starts_drain {
                    binding.writes.drain();
                }
            })
        };
        let rename = queue_change(|binding| {
            binding.rename_tag("Work", "Projects", |_| capabilities(true, false))
        });
        let delete =
            queue_change(|binding| binding.delete_tag("Old", |_| capabilities(true, false)));
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(lock_file);
        rename.join().unwrap();
        delete.join().unwrap();

        let results = results.lock().unwrap();
        assert!(
            results.len() == 2 && results.iter().all(Result::is_ok),
            "both changes apply: {results:?}"
        );
        assert_eq!(
            binding.snapshot().tags().tags_for(&tagged),
            [Box::<str>::from("Projects")]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
    }
}
