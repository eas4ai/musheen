use crate::{EventGeneration, JobEvent, JobId, Progress, ProgressUnit};
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    Planned,
    Queued,
    Running,
    Paused,
    Cancelling,
    Cancelled,
    Interrupted,
    Failed,
    Recoverable,
    Completed,
    RolledBack,
}

impl JobState {
    pub const ALL: [Self; 11] = [
        Self::Planned,
        Self::Queued,
        Self::Running,
        Self::Paused,
        Self::Cancelling,
        Self::Cancelled,
        Self::Interrupted,
        Self::Failed,
        Self::Recoverable,
        Self::Completed,
        Self::RolledBack,
    ];

    #[must_use]
    pub const fn allows(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Planned, Self::Queued | Self::Failed)
                | (
                    Self::Queued,
                    Self::Running | Self::Cancelling | Self::Cancelled | Self::Failed
                )
                | (
                    Self::Running,
                    Self::Paused
                        | Self::Cancelling
                        | Self::Failed
                        | Self::Recoverable
                        | Self::Interrupted
                        | Self::Completed
                )
                | (
                    Self::Paused,
                    Self::Running
                        | Self::Cancelling
                        | Self::Failed
                        | Self::Recoverable
                        | Self::Interrupted
                        | Self::Completed
                )
                | (
                    Self::Cancelling,
                    Self::Cancelled | Self::Failed | Self::Recoverable | Self::RolledBack
                )
                | (Self::Failed, Self::Recoverable)
                | (Self::Recoverable, Self::RolledBack)
        )
    }

    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed | Self::RolledBack)
    }
}

#[derive(Clone, Debug)]
pub struct JobStateMachine {
    id: JobId,
    generation: EventGeneration,
    state: JobState,
    progress: Option<Progress>,
    last_event_at: u64,
}

impl JobStateMachine {
    #[must_use]
    pub const fn new(id: JobId) -> Self {
        Self {
            id,
            generation: EventGeneration::new(0),
            state: JobState::Planned,
            progress: None,
            last_event_at: 0,
        }
    }

    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn generation(&self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn state(&self) -> JobState {
        self.state
    }

    #[must_use]
    pub const fn progress(&self) -> Option<Progress> {
        self.progress
    }

    pub fn apply(&mut self, event: JobEvent) -> Result<(), StateError> {
        if event.job_id() != self.id {
            return Err(StateError::WrongJob {
                expected: self.id,
                actual: event.job_id(),
            });
        }
        if event.generation() != self.generation {
            return Err(StateError::StaleGeneration {
                expected: self.generation,
                actual: event.generation(),
            });
        }
        if event.occurred_at() < self.last_event_at {
            return Err(StateError::ClockMovedBackwards {
                previous: self.last_event_at,
                actual: event.occurred_at(),
            });
        }
        if let Some(next) = event.state() {
            if !self.state.allows(next) {
                return Err(StateError::InvalidTransition {
                    from: self.state,
                    to: next,
                });
            }
            self.state = next;
        }
        if let Some(progress) = event.progress_value() {
            if !matches!(
                self.state,
                JobState::Running | JobState::Paused | JobState::Cancelling
            ) {
                return Err(StateError::ProgressUnavailable(self.state));
            }
            self.validate_progress(progress)?;
            self.progress = Some(progress);
        }
        self.last_event_at = event.occurred_at();
        Ok(())
    }

    pub fn retry(&mut self, occurred_at: u64) -> Result<EventGeneration, StateError> {
        if !matches!(
            self.state,
            JobState::Failed | JobState::Recoverable | JobState::Interrupted
        ) {
            return Err(StateError::RetryUnavailable(self.state));
        }
        if occurred_at < self.last_event_at {
            return Err(StateError::ClockMovedBackwards {
                previous: self.last_event_at,
                actual: occurred_at,
            });
        }
        self.generation = self
            .generation
            .next()
            .ok_or(StateError::GenerationExhausted)?;
        self.state = JobState::Queued;
        self.progress = None;
        self.last_event_at = occurred_at;
        Ok(self.generation)
    }

    fn validate_progress(&self, progress: Progress) -> Result<(), StateError> {
        let Some(previous) = self.progress else {
            return Ok(());
        };
        if previous.unit() != progress.unit() {
            return Err(StateError::ProgressUnitChanged {
                from: previous.unit(),
                to: progress.unit(),
            });
        }
        if progress.completed() < previous.completed() {
            return Err(StateError::ProgressRegressed {
                previous: previous.completed(),
                actual: progress.completed(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    WrongJob {
        expected: JobId,
        actual: JobId,
    },
    StaleGeneration {
        expected: EventGeneration,
        actual: EventGeneration,
    },
    InvalidTransition {
        from: JobState,
        to: JobState,
    },
    RetryUnavailable(JobState),
    ProgressUnavailable(JobState),
    GenerationExhausted,
    ClockMovedBackwards {
        previous: u64,
        actual: u64,
    },
    ProgressUnitChanged {
        from: ProgressUnit,
        to: ProgressUnit,
    },
    ProgressRegressed {
        previous: u64,
        actual: u64,
    },
}

impl fmt::Display for StateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongJob { expected, actual } => write!(
                formatter,
                "event belongs to job {}, expected {}",
                actual.get(),
                expected.get()
            ),
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "event generation {} does not match {}",
                actual.get(),
                expected.get()
            ),
            Self::InvalidTransition { from, to } => {
                write!(formatter, "invalid job transition {from:?} -> {to:?}")
            }
            Self::RetryUnavailable(state) => write!(formatter, "cannot retry a {state:?} job"),
            Self::ProgressUnavailable(state) => {
                write!(formatter, "cannot report progress for a {state:?} job")
            }
            Self::GenerationExhausted => formatter.write_str("job event generation is exhausted"),
            Self::ClockMovedBackwards { previous, actual } => write!(
                formatter,
                "event time {actual} precedes the last event time {previous}"
            ),
            Self::ProgressUnitChanged { from, to } => {
                write!(formatter, "progress unit changed from {from:?} to {to:?}")
            }
            Self::ProgressRegressed { previous, actual } => {
                write!(formatter, "progress regressed from {previous} to {actual}")
            }
        }
    }
}

impl Error for StateError {}
