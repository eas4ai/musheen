use crate::{EventGeneration, JobId, JobState};
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressUnit {
    Items,
    Bytes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveEventPhase {
    Preflight,
    Staging,
    Encoding,
    Decoding,
    Publishing,
    Cleaning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    completed: u64,
    total: Option<u64>,
    unit: ProgressUnit,
}

impl Progress {
    pub fn new(
        completed: u64,
        total: Option<u64>,
        unit: ProgressUnit,
    ) -> Result<Self, ProgressError> {
        if let Some(total) = total
            && completed > total
        {
            return Err(ProgressError::CompletedExceedsTotal { completed, total });
        }
        Ok(Self {
            completed,
            total,
            unit,
        })
    }

    #[must_use]
    pub const fn completed(self) -> u64 {
        self.completed
    }

    #[must_use]
    pub const fn total(self) -> Option<u64> {
        self.total
    }

    #[must_use]
    pub const fn unit(self) -> ProgressUnit {
        self.unit
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressError {
    CompletedExceedsTotal { completed: u64, total: u64 },
}

impl fmt::Display for ProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CompletedExceedsTotal { completed, total } => {
                write!(
                    formatter,
                    "completed progress {completed} exceeds total {total}"
                )
            }
        }
    }
}

impl Error for ProgressError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobEvent {
    job_id: JobId,
    generation: EventGeneration,
    occurred_at: u64,
    state: Option<JobState>,
    progress: Option<Progress>,
    archive_phase: Option<ArchiveEventPhase>,
}

impl JobEvent {
    #[must_use]
    pub const fn transition(
        job_id: JobId,
        generation: EventGeneration,
        occurred_at: u64,
        state: JobState,
    ) -> Self {
        Self {
            job_id,
            generation,
            occurred_at,
            state: Some(state),
            progress: None,
            archive_phase: None,
        }
    }

    #[must_use]
    pub const fn progress(
        job_id: JobId,
        generation: EventGeneration,
        occurred_at: u64,
        progress: Progress,
    ) -> Self {
        Self {
            job_id,
            generation,
            occurred_at,
            state: None,
            progress: Some(progress),
            archive_phase: None,
        }
    }

    #[must_use]
    pub const fn archive_phase(
        job_id: JobId,
        generation: EventGeneration,
        occurred_at: u64,
        archive_phase: ArchiveEventPhase,
    ) -> Self {
        Self {
            job_id,
            generation,
            occurred_at,
            state: None,
            progress: None,
            archive_phase: Some(archive_phase),
        }
    }

    #[must_use]
    pub const fn job_id(&self) -> JobId {
        self.job_id
    }

    #[must_use]
    pub const fn generation(&self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn occurred_at(&self) -> u64 {
        self.occurred_at
    }

    #[must_use]
    pub const fn state(&self) -> Option<JobState> {
        self.state
    }

    #[must_use]
    pub const fn progress_value(&self) -> Option<Progress> {
        self.progress
    }

    #[must_use]
    pub const fn archive_phase_value(&self) -> Option<ArchiveEventPhase> {
        self.archive_phase
    }
}
