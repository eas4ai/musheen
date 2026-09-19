use crate::metadata::{io_error, item_from_dir_entry};
use musheen_core::{
    CancellationToken, Continuation, Page, PageRequest, ProviderId, StoreError, StoreItem,
    StorePath, TotalHint,
};
use std::collections::HashMap;
use std::fs::{self, DirEntry, ReadDir};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_OPEN_ENUMERATIONS: usize = 16;

pub(crate) struct EnumerationRegistry {
    provider: ProviderId,
    next_id: AtomicU64,
    sessions: Mutex<HashMap<u64, EnumerationSession>>,
}

struct EnumerationSession {
    location: StorePath,
    entries: ReadDir,
    pending: Option<DirEntry>,
    returned: u64,
}

impl EnumerationRegistry {
    pub(crate) fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            next_id: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn read_page(
        &self,
        location: &StorePath,
        request: PageRequest,
        cancellation: &CancellationToken,
    ) -> Result<Page<StoreItem>, StoreError> {
        let continuation_id = decode_continuation(&request)?;
        self.reject_cancelled(continuation_id, cancellation)?;
        let (session_id, mut session) = self.take_session(location, continuation_id)?;
        let items = self.collect_items(location, &request, cancellation, &mut session)?;
        let (next, total_hint) = self.finish_page(location, session_id, session, cancellation)?;

        Page::try_new(&request, items, next, total_hint)
    }

    fn reject_cancelled(
        &self,
        continuation_id: Option<u64>,
        cancellation: &CancellationToken,
    ) -> Result<(), StoreError> {
        if !cancellation.is_cancelled() {
            return Ok(());
        }
        if let Some(session_id) = continuation_id {
            self.lock_sessions()?.remove(&session_id);
        }
        Err(StoreError::Cancelled)
    }

    fn take_session(
        &self,
        location: &StorePath,
        continuation_id: Option<u64>,
    ) -> Result<(u64, EnumerationSession), StoreError> {
        let Some(session_id) = continuation_id else {
            return self.start_session(location);
        };
        let session = self
            .lock_sessions()?
            .remove(&session_id)
            .ok_or(StoreError::InvalidContinuation)?;
        if session.location != *location {
            return Err(StoreError::InvalidContinuation);
        }
        Ok((session_id, session))
    }

    fn collect_items(
        &self,
        location: &StorePath,
        request: &PageRequest,
        cancellation: &CancellationToken,
        session: &mut EnumerationSession,
    ) -> Result<Vec<StoreItem>, StoreError> {
        let mut items = Vec::with_capacity(request.page_size());
        while items.len() < request.page_size() {
            cancellation.check()?;
            let Some(entry) = session
                .next_entry()
                .map_err(|error| io_error("read directory entry", Some(location.clone()), error))?
            else {
                break;
            };
            items.push(item_from_dir_entry(&self.provider, &entry)?);
            session.returned = session.returned.saturating_add(1);
        }
        Ok(items)
    }

    fn finish_page(
        &self,
        location: &StorePath,
        session_id: u64,
        mut session: EnumerationSession,
        cancellation: &CancellationToken,
    ) -> Result<(Option<Continuation>, TotalHint), StoreError> {
        cancellation.check()?;
        session.prefetch(location)?;
        if session.pending.is_none() {
            return Ok((None, TotalHint::Exact(session.returned)));
        }

        let total_hint = TotalHint::AtLeast(session.returned);
        self.insert_session(session_id, session)?;
        let continuation = usize::try_from(session_id)
            .map(Continuation::from_usize)
            .map_err(|_| StoreError::InvalidContinuation)?;
        Ok((Some(continuation), total_hint))
    }

    fn start_session(&self, location: &StorePath) -> Result<(u64, EnumerationSession), StoreError> {
        let path = location.as_unix_path().ok_or_else(|| {
            StoreError::unsupported(
                "read_directory",
                "the local provider accepts only Unix paths",
            )
        })?;
        let entries = fs::read_dir(path)
            .map_err(|error| io_error("open directory", Some(location.clone()), error))?;
        let session_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if session_id == 0 {
            return Err(StoreError::Backend(
                "directory enumeration identifier space exhausted".into(),
            ));
        }
        Ok((
            session_id,
            EnumerationSession {
                location: location.clone(),
                entries,
                pending: None,
                returned: 0,
            },
        ))
    }

    fn insert_session(
        &self,
        session_id: u64,
        session: EnumerationSession,
    ) -> Result<(), StoreError> {
        let mut sessions = self.lock_sessions()?;
        if sessions.len() >= MAX_OPEN_ENUMERATIONS
            && let Some(oldest) = sessions.keys().min().copied()
        {
            sessions.remove(&oldest);
        }
        sessions.insert(session_id, session);
        Ok(())
    }

    fn lock_sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<u64, EnumerationSession>>, StoreError> {
        self.sessions
            .lock()
            .map_err(|_| StoreError::Backend("enumeration registry is poisoned".into()))
    }
}

impl EnumerationSession {
    fn next_entry(&mut self) -> std::io::Result<Option<DirEntry>> {
        if let Some(entry) = self.pending.take() {
            return Ok(Some(entry));
        }
        self.entries.next().transpose()
    }

    fn prefetch(&mut self, location: &StorePath) -> Result<(), StoreError> {
        self.pending = self.entries.next().transpose().map_err(|error| {
            io_error(
                "continue directory enumeration",
                Some(location.clone()),
                error,
            )
        })?;
        Ok(())
    }
}

fn decode_continuation(request: &PageRequest) -> Result<Option<u64>, StoreError> {
    request
        .continuation()
        .map(Continuation::decode_usize)
        .transpose()?
        .map(|id| u64::try_from(id).map_err(|_| StoreError::InvalidContinuation))
        .transpose()
}
