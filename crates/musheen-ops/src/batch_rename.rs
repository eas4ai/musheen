use crate::create::{sibling_path, validate_local_name};
use crate::rename::validate_source;
use crate::{MutationError, MutationProvider};
use musheen_core::StorePath;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

const TEMPORARY_PREFIX: &str = ".musheen-rename-v1-";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameMapping {
    source: StorePath,
    target_name: OsString,
    expected_identity: Box<[u8]>,
}

impl RenameMapping {
    #[must_use]
    pub fn new(source: StorePath, target_name: OsString, expected_identity: Vec<u8>) -> Self {
        Self {
            source,
            target_name,
            expected_identity: expected_identity.into_boxed_slice(),
        }
    }

    fn destination(&self) -> Result<StorePath, MutationError> {
        sibling_path(&self.source, &self.target_name)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchRenameStep {
    source: StorePath,
    destination: StorePath,
    expected_identity: Box<[u8]>,
    temporary: bool,
}

impl BatchRenameStep {
    #[must_use]
    pub const fn source(&self) -> &StorePath {
        &self.source
    }

    #[must_use]
    pub const fn destination(&self) -> &StorePath {
        &self.destination
    }

    #[must_use]
    pub const fn expected_identity(&self) -> &[u8] {
        &self.expected_identity
    }

    #[must_use]
    pub const fn is_temporary(&self) -> bool {
        self.temporary
    }
}

pub trait BatchRenameJournal {
    /// Durably records the complete step list before the first rename.
    fn persist_plan(&mut self, steps: &[BatchRenameStep]) -> Result<(), MutationError>;

    /// Durably records completion of one step before execution advances.
    fn persist_completed_step(&mut self, index: usize) -> Result<(), MutationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRenamePlan {
    steps: Vec<BatchRenameStep>,
}

impl BatchRenamePlan {
    pub fn preflight(
        provider: &mut impl MutationProvider,
        mappings: Vec<RenameMapping>,
    ) -> Result<Self, MutationError> {
        if mappings.is_empty() {
            return Err(MutationError::InvalidScope);
        }
        let mut sources = HashSet::with_capacity(mappings.len());
        let mut destinations = HashSet::with_capacity(mappings.len());
        let mut pending = Vec::with_capacity(mappings.len());

        for mapping in mappings {
            validate_local_name(&mapping.target_name).map_err(|_| MutationError::InvalidName)?;
            let destination = mapping.destination()?;
            if !provider.allows_rename(&mapping.source)? {
                return Err(MutationError::Unsupported);
            }
            if !sources.insert(mapping.source.clone()) {
                return Err(MutationError::BatchCollision);
            }
            validate_source(provider, &mapping.source, &mapping.expected_identity)?;
            if !destinations.insert(destination.clone()) {
                return Err(MutationError::BatchCollision);
            }
            pending.push(BatchRenameStep {
                source: mapping.source,
                destination,
                expected_identity: mapping.expected_identity,
                temporary: false,
            });
        }

        for rename in &pending {
            if rename.source == rename.destination {
                continue;
            }
            if let Some(identity) = provider.identity(&rename.destination)?
                && !sources.contains(&rename.destination)
                && identity.as_ref() != rename.expected_identity.as_ref()
            {
                return Err(MutationError::Conflict);
            }
        }

        let steps = plan_steps(provider, pending, &sources, &destinations)?;
        Ok(Self { steps })
    }

    pub fn execute(
        self,
        provider: &mut impl MutationProvider,
        journal: &mut impl BatchRenameJournal,
    ) -> Result<(), MutationError> {
        journal.persist_plan(&self.steps)?;
        for (index, rename) in self.steps.into_iter().enumerate() {
            provider.rename_no_replace(
                &rename.source,
                &rename.destination,
                &rename.expected_identity,
            )?;
            journal.persist_completed_step(index)?;
        }
        Ok(())
    }
}

fn plan_steps(
    provider: &mut impl MutationProvider,
    mut pending: Vec<BatchRenameStep>,
    sources: &HashSet<StorePath>,
    destinations: &HashSet<StorePath>,
) -> Result<Vec<BatchRenameStep>, MutationError> {
    pending.retain(|rename| rename.source != rename.destination);
    let mut reserved = sources.union(destinations).cloned().collect();
    let mut temporary_sequence = 0_u64;
    let mut steps = Vec::with_capacity(pending.len());

    while !pending.is_empty() {
        let current_sources: HashSet<_> =
            pending.iter().map(|rename| rename.source.clone()).collect();
        if let Some(index) = pending
            .iter()
            .position(|rename| !current_sources.contains(&rename.destination))
        {
            steps.push(pending.remove(index));
            continue;
        }

        let cycle = &pending[0];
        let temporary =
            next_temporary_path(provider, &cycle.source, &mut temporary_sequence, &reserved)?;
        reserved.insert(temporary.clone());
        steps.push(BatchRenameStep {
            source: cycle.source.clone(),
            destination: temporary.clone(),
            expected_identity: cycle.expected_identity.clone(),
            temporary: true,
        });
        pending[0].source = temporary;
    }
    Ok(steps)
}

fn next_temporary_path(
    provider: &mut impl MutationProvider,
    source: &StorePath,
    sequence: &mut u64,
    reserved: &HashSet<StorePath>,
) -> Result<StorePath, MutationError> {
    loop {
        let name = OsString::from(format!("{TEMPORARY_PREFIX}{}", *sequence));
        *sequence = sequence
            .checked_add(1)
            .ok_or(MutationError::BatchCollision)?;
        let candidate = sibling_path(source, OsStr::new(&name))?;
        if !reserved.contains(&candidate) && provider.identity(&candidate)?.is_none() {
            return Ok(candidate);
        }
    }
}
