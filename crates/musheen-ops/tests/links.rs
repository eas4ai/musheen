use musheen_core::StorePath;
use musheen_ops::{
    HardLinkRequest, LinkProvider, MutationError, SymbolicLinkRequest, execute_hard_link,
    execute_symbolic_link,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;

struct RecordingProvider {
    identities: HashMap<StorePath, Box<[u8]>>,
    filesystems: HashMap<StorePath, u64>,
    symbolic_links: Vec<(OsString, StorePath)>,
    hard_links: Vec<(StorePath, StorePath)>,
    occupied: HashSet<StorePath>,
    symbolic_allowed: bool,
    hard_allowed: bool,
}

impl Default for RecordingProvider {
    fn default() -> Self {
        Self {
            identities: HashMap::new(),
            filesystems: HashMap::new(),
            symbolic_links: Vec::new(),
            hard_links: Vec::new(),
            occupied: HashSet::new(),
            symbolic_allowed: true,
            hard_allowed: true,
        }
    }
}

impl LinkProvider for RecordingProvider {
    fn allows_symbolic_links(&mut self, _parent: &StorePath) -> Result<bool, MutationError> {
        Ok(self.symbolic_allowed)
    }

    fn allows_hard_links(
        &mut self,
        _source: &StorePath,
        _parent: &StorePath,
    ) -> Result<bool, MutationError> {
        Ok(self.hard_allowed)
    }

    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        Ok(self.identities.get(path).cloned())
    }

    fn filesystem_id(&mut self, path: &StorePath) -> Result<u64, MutationError> {
        self.filesystems
            .get(path)
            .copied()
            .ok_or(MutationError::Missing)
    }

    fn create_symbolic_link(
        &mut self,
        target: &OsStr,
        destination: &StorePath,
    ) -> Result<(), MutationError> {
        if !self.occupied.insert(destination.clone()) {
            return Err(MutationError::Conflict);
        }
        self.symbolic_links
            .push((target.to_os_string(), destination.clone()));
        Ok(())
    }

    fn create_hard_link(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
        expected_identity: &[u8],
    ) -> Result<(), MutationError> {
        if self.identities.get(source).map(Box::as_ref) != Some(expected_identity) {
            return Err(MutationError::SourceChanged);
        }
        if !self.occupied.insert(destination.clone()) {
            return Err(MutationError::Conflict);
        }
        self.hard_links.push((source.clone(), destination.clone()));
        Ok(())
    }
}

#[test]
fn symbolic_links_preserve_lossless_relative_targets_and_refuse_conflicts() {
    let mut provider = RecordingProvider::default();
    let target = OsString::from_vec(vec![b'.', b'.', b'/', b'n', 0xff]);
    let request = SymbolicLinkRequest::new(local("/work"), OsString::from("link"), target.clone());

    execute_symbolic_link(&mut provider, &request).unwrap();
    assert_eq!(provider.symbolic_links[0].0, target);
    assert_eq!(
        execute_symbolic_link(&mut provider, &request),
        Err(MutationError::Conflict)
    );

    let mut unsupported = RecordingProvider {
        symbolic_allowed: false,
        ..RecordingProvider::default()
    };
    assert_eq!(
        execute_symbolic_link(&mut unsupported, &request),
        Err(MutationError::Unsupported)
    );
    assert!(unsupported.symbolic_links.is_empty());
}

#[test]
fn hard_links_preflight_identity_and_filesystem_before_mutation() {
    let source = local("/source/file");
    let parent = local("/destination");
    let mut provider = RecordingProvider::default();
    provider
        .identities
        .insert(source.clone(), b"id".to_vec().into());
    provider.filesystems.insert(source.clone(), 1);
    provider.filesystems.insert(parent.clone(), 2);
    let request = HardLinkRequest::new(
        source.clone(),
        parent.clone(),
        OsString::from("linked"),
        b"id".to_vec(),
    );

    assert_eq!(
        execute_hard_link(&mut provider, &request),
        Err(MutationError::CrossFilesystem)
    );
    assert!(provider.hard_links.is_empty());

    provider.filesystems.insert(parent, 1);
    provider
        .identities
        .insert(source, b"changed".to_vec().into());
    assert_eq!(
        execute_hard_link(&mut provider, &request),
        Err(MutationError::SourceChanged)
    );
    assert!(provider.hard_links.is_empty());
}

fn local(path: &str) -> StorePath {
    StorePath::from_unix_path(path)
}
