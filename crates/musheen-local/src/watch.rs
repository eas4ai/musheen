use crate::metadata::{io_error, item_from_path};
use musheen_core::{
    BoxFuture, CancellationToken, DirectoryWatch, ItemId, ProviderId, StoreError, StoreItem,
    StorePath, WatchEvent, WatchFailure, WatchSemantics,
};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

const MAX_TRACKED_IDENTITIES: usize = 4_096;

pub(crate) struct LocalWatch {
    provider: ProviderId,
    location: StorePath,
    queue: Arc<EventQueue>,
    identities: HashMap<PathBuf, ItemId>,
    _watcher: RecommendedWatcher,
}

#[derive(Default)]
struct EventQueue {
    state: Mutex<EventQueueState>,
}

#[derive(Default)]
struct EventQueueState {
    events: VecDeque<notify::Result<Event>>,
    waker: Option<Waker>,
}

impl EventQueue {
    fn push(&self, event: notify::Result<Event>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.events.push_back(event);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

impl LocalWatch {
    pub(crate) fn open(
        provider: ProviderId,
        location: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<Self, StoreError> {
        cancellation.check()?;
        let path = location.as_unix_path().ok_or_else(|| {
            StoreError::unsupported(
                "watch_directory",
                "the local provider accepts only Unix paths",
            )
        })?;
        let queue = Arc::new(EventQueue::default());
        let callback_queue = Arc::clone(&queue);
        let mut watcher = notify::recommended_watcher(move |event| callback_queue.push(event))
            .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        watcher
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        let identities = snapshot_identities(&provider, path)?;
        Ok(Self {
            provider,
            location: location.clone(),
            queue,
            identities,
            _watcher: watcher,
        })
    }

    fn translate(
        &mut self,
        event: notify::Result<Event>,
    ) -> Result<Option<WatchEvent>, StoreError> {
        let event = match event {
            Ok(event) => event,
            Err(_) => {
                return Ok(Some(WatchEvent::invalidation(
                    self.location.clone(),
                    WatchFailure::EventGap,
                )));
            }
        };
        if event.need_rescan() {
            return Ok(Some(WatchEvent::invalidation(
                self.location.clone(),
                WatchFailure::Overflow,
            )));
        }

        match event.kind {
            EventKind::Access(_) => Ok(None),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() >= 2 => {
                let previous_path = event.paths[0].clone();
                let current_path = event.paths[event.paths.len() - 1].clone();
                let item = item_from_path(&self.provider, &current_path)?;
                self.identities.remove(&previous_path);
                self.remember_item(&item)?;
                Ok(Some(WatchEvent::Renamed {
                    previous_path: StorePath::from_unix_path(previous_path.into_os_string()),
                    item,
                }))
            }
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                self.removed_event(event.paths.first())
            }
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                self.item_event(event.paths.last(), true)
            }
            EventKind::Modify(_) => self.item_event(event.paths.last(), false),
            EventKind::Any | EventKind::Other => Ok(Some(WatchEvent::invalidation(
                self.location.clone(),
                WatchFailure::EventGap,
            ))),
        }
    }

    fn item_event(
        &mut self,
        path: Option<&PathBuf>,
        created: bool,
    ) -> Result<Option<WatchEvent>, StoreError> {
        let Some(path) = path else {
            return Ok(Some(WatchEvent::invalidation(
                self.location.clone(),
                WatchFailure::EventGap,
            )));
        };
        let item = match item_from_path(&self.provider, path) {
            Ok(item) => item,
            Err(StoreError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => return self.removed_event(Some(path)),
            Err(error) => return Err(error),
        };
        self.remember_item(&item)?;
        Ok(Some(if created {
            WatchEvent::Created(item)
        } else {
            WatchEvent::Changed(item)
        }))
    }

    fn removed_event(&mut self, path: Option<&PathBuf>) -> Result<Option<WatchEvent>, StoreError> {
        let Some(path) = path else {
            return Ok(Some(WatchEvent::invalidation(
                self.location.clone(),
                WatchFailure::EventGap,
            )));
        };
        Ok(Some(match self.identities.remove(path) {
            Some(id) => WatchEvent::Removed(id),
            None => WatchEvent::invalidation(self.location.clone(), WatchFailure::EventGap),
        }))
    }

    fn remember(&mut self, path: &Path, id: ItemId) {
        if self.identities.len() < MAX_TRACKED_IDENTITIES || self.identities.contains_key(path) {
            self.identities.insert(path.to_path_buf(), id);
        }
    }

    fn remember_item(&mut self, item: &StoreItem) -> Result<(), StoreError> {
        let path = item.path().as_unix_path().ok_or_else(|| {
            StoreError::Backend("the local watcher produced a non-Unix item path".into())
        })?;
        self.remember(path, item.id().clone());
        Ok(())
    }
}

impl DirectoryWatch for LocalWatch {
    fn semantics(&self) -> WatchSemantics {
        WatchSemantics::Live
    }

    fn next_event<'a>(
        &'a mut self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<WatchEvent, StoreError>> {
        Box::pin(async move {
            poll_fn(|context| {
                if let Err(error) = cancellation.check() {
                    return Poll::Ready(Err(error));
                }
                let next = {
                    let mut state = self
                        .queue
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match state.events.pop_front() {
                        Some(event) => Some(event),
                        None => {
                            state.waker = Some(context.waker().clone());
                            None
                        }
                    }
                };
                if let Some(event) = next {
                    match self.translate(event) {
                        Ok(Some(event)) => Poll::Ready(Ok(event)),
                        Ok(None) => {
                            context.waker().wake_by_ref();
                            Poll::Pending
                        }
                        Err(error) => Poll::Ready(Err(error)),
                    }
                } else {
                    cancellation.register_waker(context.waker());
                    Poll::Pending
                }
            })
            .await
        })
    }
}

fn snapshot_identities(
    provider: &ProviderId,
    directory: &Path,
) -> Result<HashMap<PathBuf, ItemId>, StoreError> {
    let mut identities = HashMap::new();
    let entries = std::fs::read_dir(directory).map_err(|error| {
        io_error(
            "snapshot watched directory",
            Some(StorePath::from_unix_path(
                directory.as_os_str().to_os_string(),
            )),
            error,
        )
    })?;
    for entry in entries.take(MAX_TRACKED_IDENTITIES) {
        let entry = entry.map_err(|error| io_error("snapshot watched entry", None, error))?;
        let item = crate::metadata::item_from_dir_entry(provider, &entry)?;
        identities.insert(entry.path(), item.id().clone());
    }
    Ok(identities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::Flag;

    #[test]
    fn notify_rescan_flag_maps_to_overflow_invalidation() {
        let directory = tempfile::tempdir().expect("the temporary directory is created");
        let provider = ProviderId::new("local").expect("the provider ID is valid");
        let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
        let cancellation = CancellationToken::new();
        let mut watch =
            LocalWatch::open(provider, &location, &cancellation).expect("the local watch opens");
        let event = Event::new(EventKind::Other).set_flag(Flag::Rescan);

        assert!(matches!(
            watch.translate(Ok(event)),
            Ok(Some(WatchEvent::Invalidated {
                cause: WatchFailure::Overflow,
                ..
            }))
        ));
    }
}
