use crate::operation_journal::{
    append_private, create_private_directory, open_private_append, sync_directory,
};
use musheen_core::StorePath;
use musheen_ops::{
    ConflictChoice, ConflictDecision, ConflictDecisionJournal, MutationError, OperationKind,
};
use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};

const CONFLICT_JOURNAL_FILE: &str = "conflicts.journal";

pub struct ConflictDecisionStore {
    path: PathBuf,
    directory: PathBuf,
}

impl ConflictDecisionStore {
    pub fn for_current_user() -> io::Result<Self> {
        Self::from_config_home(freedesktop::xdg_config_home())
    }

    pub fn from_config_home(config_home: impl AsRef<Path>) -> io::Result<Self> {
        Self::at(
            config_home
                .as_ref()
                .join("musheen")
                .join(CONFLICT_JOURNAL_FILE),
        )
    }

    pub fn at(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal needs a parent"))?
            .to_path_buf();
        create_private_directory(&directory)?;
        Ok(Self { path, directory })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ConflictDecisionJournal for ConflictDecisionStore {
    fn persist_decision(&mut self, decision: &ConflictDecision) -> Result<(), MutationError> {
        let mut document = serde_json::to_vec(&DecisionDocument::from(decision))
            .map_err(|error| MutationError::Provider(error.to_string().into()))?;
        document.push(b'\n');
        append_private(&self.path, &document).map_err(journal_error)?;
        open_private_append(&self.path)
            .and_then(|file| file.sync_all())
            .map_err(journal_error)?;
        sync_directory(&self.directory).map_err(journal_error)
    }
}

#[derive(Serialize)]
struct DecisionDocument<'a> {
    schema: u32,
    operation: &'static str,
    source: &'a StorePath,
    source_identity: &'a [u8],
    destination: &'a StorePath,
    destination_identity: &'a [u8],
    choice: &'static str,
}

impl<'a> From<&'a ConflictDecision> for DecisionDocument<'a> {
    fn from(decision: &'a ConflictDecision) -> Self {
        Self {
            schema: 1,
            operation: operation_name(decision.operation()),
            source: decision.source(),
            source_identity: decision.source_identity(),
            destination: decision.destination(),
            destination_identity: decision.destination_identity(),
            choice: choice_name(decision.choice()),
        }
    }
}

fn journal_error(error: io::Error) -> MutationError {
    MutationError::Provider(format!("conflict journal: {error}").into())
}

const fn choice_name(choice: ConflictChoice) -> &'static str {
    match choice {
        ConflictChoice::Replace => "replace",
        ConflictChoice::Skip => "skip",
        ConflictChoice::KeepBoth => "keep_both",
        ConflictChoice::MergeDirectory => "merge_directory",
        ConflictChoice::ReplaceTree => "replace_tree",
    }
}

const fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::Copy => "copy",
        OperationKind::Move => "move",
        OperationKind::Trash => "trash",
        OperationKind::PermanentDelete => "permanent_delete",
        OperationKind::CreateFile => "create_file",
        OperationKind::CreateDirectory => "create_directory",
        OperationKind::Rename => "rename",
        OperationKind::SymbolicLink => "symbolic_link",
        OperationKind::HardLink => "hard_link",
        OperationKind::SetPermissions => "set_permissions",
        OperationKind::SetOwnership => "set_ownership",
        OperationKind::SetExtendedAttribute => "set_extended_attribute",
        OperationKind::Compress => "compress",
        OperationKind::Extract => "extract",
        OperationKind::Restore => "restore",
        OperationKind::Hash => "hash",
        OperationKind::Preview => "preview",
    }
}
