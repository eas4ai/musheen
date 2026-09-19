use musheen_core::{CancellationToken, StorePath};
use musheen_ops::{
    CopyCapabilities, CopyProvider, CopyRequest, EntryKind, EntrySnapshot, EventGeneration, JobId,
    MetadataReport, ProviderError,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Inspect { follow_links: bool },
    CreateStaging,
    HardLink,
    Reflink,
    Sparse,
    Stream,
    Symlink,
    Directory,
    Metadata,
    Verify,
    Publish,
    Cleanup,
    AtomicMove,
    RemoveSource,
}

pub struct RecordingProvider {
    pub capabilities: CopyCapabilities,
    pub initial: EntrySnapshot,
    pub followed: Option<EntrySnapshot>,
    pub after: Option<EntrySnapshot>,
    pub metadata: MetadataReport,
    pub actions: Vec<Action>,
    pub fail_action: Option<Action>,
    pub fail_with: ProviderError,
    pub verify_ok: bool,
    pub reflink_ok: bool,
    pub sparse_ok: bool,
    pub hard_link_ok: bool,
    pub atomic_move_ok: bool,
    pub cancel_after_publish: bool,
    pub cleanup_fails: bool,
    inspections: usize,
}

impl RecordingProvider {
    pub fn regular() -> Self {
        Self {
            capabilities: CopyCapabilities::default(),
            initial: EntrySnapshot::new(b"source".to_vec(), EntryKind::RegularFile, 8, 8, 1),
            followed: None,
            after: None,
            metadata: MetadataReport::default(),
            actions: Vec::new(),
            fail_action: None,
            fail_with: ProviderError::Other("injected failure".into()),
            verify_ok: true,
            reflink_ok: false,
            sparse_ok: false,
            hard_link_ok: false,
            atomic_move_ok: false,
            cancel_after_publish: false,
            cleanup_fails: false,
            inspections: 0,
        }
    }

    fn record(&mut self, action: Action) -> Result<(), ProviderError> {
        self.actions.push(action.clone());
        if self.fail_action.as_ref() == Some(&action) {
            Err(self.fail_with.clone())
        } else {
            Ok(())
        }
    }
}

impl CopyProvider for RecordingProvider {
    fn capabilities(&self, _source: &StorePath, _destination: &StorePath) -> CopyCapabilities {
        self.capabilities
    }

    fn inspect(
        &mut self,
        _path: &StorePath,
        follow_links: bool,
    ) -> Result<EntrySnapshot, ProviderError> {
        self.record(Action::Inspect { follow_links })?;
        self.inspections += 1;
        if follow_links && let Some(followed) = &self.followed {
            return Ok(followed.clone());
        }
        if self.inspections > 1 {
            Ok(self.after.clone().unwrap_or_else(|| self.initial.clone()))
        } else {
            Ok(self.initial.clone())
        }
    }

    fn create_staging(
        &mut self,
        _staging: &StorePath,
        _kind: EntryKind,
    ) -> Result<(), ProviderError> {
        self.record(Action::CreateStaging)
    }

    fn try_hard_link(
        &mut self,
        _existing: &StorePath,
        _staging: &StorePath,
    ) -> Result<bool, ProviderError> {
        self.record(Action::HardLink)?;
        Ok(self.hard_link_ok)
    }

    fn try_reflink(
        &mut self,
        _source: &StorePath,
        _staging: &StorePath,
    ) -> Result<bool, ProviderError> {
        self.record(Action::Reflink)?;
        Ok(self.reflink_ok)
    }

    fn try_sparse_copy(
        &mut self,
        _source: &StorePath,
        _staging: &StorePath,
        _cancellation: &CancellationToken,
    ) -> Result<Option<u64>, ProviderError> {
        self.record(Action::Sparse)?;
        Ok(self.sparse_ok.then_some(self.initial.size()))
    }

    fn copy_streamed(
        &mut self,
        _source: &StorePath,
        _staging: &StorePath,
        _cancellation: &CancellationToken,
    ) -> Result<u64, ProviderError> {
        self.record(Action::Stream)?;
        Ok(self.initial.size())
    }

    fn copy_symlink(
        &mut self,
        _source: &StorePath,
        _staging: &StorePath,
    ) -> Result<(), ProviderError> {
        self.record(Action::Symlink)
    }

    fn copy_directory(
        &mut self,
        _source: &StorePath,
        _staging: &StorePath,
        _include_nested_mounts: bool,
        _cancellation: &CancellationToken,
    ) -> Result<u64, ProviderError> {
        self.record(Action::Directory)?;
        Ok(self.initial.size())
    }

    fn apply_metadata(
        &mut self,
        _source: &StorePath,
        _source_snapshot: &EntrySnapshot,
        _staging: &StorePath,
    ) -> Result<MetadataReport, ProviderError> {
        self.record(Action::Metadata)?;
        Ok(self.metadata.clone())
    }

    fn verify(
        &mut self,
        _source_path: &StorePath,
        _source: &EntrySnapshot,
        _staging: &StorePath,
        _metadata: &MetadataReport,
    ) -> Result<bool, ProviderError> {
        self.record(Action::Verify)?;
        Ok(self.verify_ok)
    }

    fn publish(
        &mut self,
        _staging: &StorePath,
        _destination: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<(), ProviderError> {
        self.record(Action::Publish)?;
        if self.cancel_after_publish {
            cancellation.cancel();
        }
        Ok(())
    }

    fn cleanup_staging(&mut self, _staging: &StorePath) -> Result<(), ProviderError> {
        self.record(Action::Cleanup)?;
        if self.cleanup_fails {
            Err(ProviderError::Other("cleanup failed".into()))
        } else {
            Ok(())
        }
    }

    fn try_atomic_move(
        &mut self,
        _source: &StorePath,
        _destination: &StorePath,
    ) -> Result<bool, ProviderError> {
        self.record(Action::AtomicMove)?;
        Ok(self.atomic_move_ok)
    }

    fn remove_source(
        &mut self,
        _source: &StorePath,
        _expected: &EntrySnapshot,
    ) -> Result<(), ProviderError> {
        self.record(Action::RemoveSource)
    }
}

pub fn source() -> StorePath {
    StorePath::from_unix_path("/source/item")
}

pub fn destination(name: &str) -> StorePath {
    StorePath::from_unix_path(format!("/destination/{name}"))
}

pub fn request(name: &str) -> CopyRequest {
    CopyRequest::new(
        JobId::new(1).unwrap(),
        EventGeneration::new(0),
        source(),
        destination(name),
    )
}
