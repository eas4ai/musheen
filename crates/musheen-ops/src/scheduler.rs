use crate::{
    EventGeneration, JobEvent, JobId, JobState, JobStateMachine, OperationPlan, StateError,
    WorkClass,
};
use musheen_core::{CancellationToken, ResourceLimits};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::time::Instant;

pub trait Clock {
    fn now(&self) -> u64;
}

#[derive(Clone, Debug)]
pub struct SystemClock {
    started: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Debug)]
pub struct ScheduledJob {
    id: JobId,
    plan: OperationPlan,
    cancellation: CancellationToken,
    generation: EventGeneration,
    started_at: u64,
}

impl ScheduledJob {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn plan(&self) -> &OperationPlan {
        &self.plan
    }

    #[must_use]
    pub const fn class(&self) -> WorkClass {
        self.plan.class()
    }

    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    #[must_use]
    pub const fn generation(&self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn started_at(&self) -> u64 {
        self.started_at
    }
}

#[derive(Clone, Debug)]
struct JobRecord {
    plan: OperationPlan,
    state: JobStateMachine,
    cancellation: CancellationToken,
}

#[derive(Debug)]
pub struct Scheduler<C = SystemClock> {
    limits: ResourceLimits,
    clock: C,
    next_job_id: u64,
    queued: VecDeque<JobId>,
    running: BTreeSet<JobId>,
    jobs: BTreeMap<JobId, JobRecord>,
    events: Vec<JobEvent>,
}

impl Scheduler<SystemClock> {
    #[must_use]
    pub fn new(limits: &ResourceLimits) -> Self {
        Self::with_clock(limits, SystemClock::default())
    }
}

impl<C: Clock> Scheduler<C> {
    #[must_use]
    pub fn with_clock(limits: &ResourceLimits, clock: C) -> Self {
        Self {
            limits: limits.snapshot(),
            clock,
            next_job_id: 1,
            queued: VecDeque::new(),
            running: BTreeSet::new(),
            jobs: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    pub fn enqueue(&mut self, plan: OperationPlan) -> Result<JobId, SchedulerError> {
        let id = JobId::new(self.next_job_id).ok_or(SchedulerError::JobIdExhausted)?;
        self.next_job_id = self
            .next_job_id
            .checked_add(1)
            .ok_or(SchedulerError::JobIdExhausted)?;
        let cancellation = CancellationToken::new();
        let state = JobStateMachine::new(id);
        self.jobs.insert(
            id,
            JobRecord {
                plan,
                state,
                cancellation,
            },
        );
        self.transition(id, JobState::Queued)?;
        self.queued.push_back(id);
        Ok(id)
    }

    pub fn start_ready(&mut self) -> Result<Vec<ScheduledJob>, SchedulerError> {
        let queued = self.queued.len();
        let mut started = Vec::new();
        for _ in 0..queued {
            let id = self
                .queued
                .pop_front()
                .expect("the recorded queue length remains exact");
            if !self.can_start(id)? {
                self.queued.push_back(id);
                continue;
            }
            let started_at = self.clock.now();
            self.transition_at(id, JobState::Running, started_at)?;
            self.running.insert(id);
            let record = self.jobs.get(&id).ok_or(SchedulerError::UnknownJob(id))?;
            started.push(ScheduledJob {
                id,
                plan: record.plan.clone(),
                cancellation: record.cancellation.clone(),
                generation: record.state.generation(),
                started_at,
            });
        }
        Ok(started)
    }

    pub fn complete(&mut self, id: JobId) -> Result<(), SchedulerError> {
        if !self.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        self.transition(id, JobState::Completed)?;
        self.running.remove(&id);
        Ok(())
    }

    pub fn cancel(&mut self, id: JobId) -> Result<(), SchedulerError> {
        let state = self.state(id).ok_or(SchedulerError::UnknownJob(id))?;
        let cancellation = self
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .cancellation
            .clone();
        cancellation.cancel();
        self.transition(id, JobState::Cancelling)?;
        if state == JobState::Queued {
            self.queued.retain(|queued| *queued != id);
            self.transition(id, JobState::RolledBack)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn state(&self, id: JobId) -> Option<JobState> {
        self.jobs.get(&id).map(|record| record.state.state())
    }

    #[must_use]
    pub const fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    #[must_use]
    pub fn events(&self) -> &[JobEvent] {
        &self.events
    }

    fn can_start(&self, id: JobId) -> Result<bool, SchedulerError> {
        let candidate = self.jobs.get(&id).ok_or(SchedulerError::UnknownJob(id))?;
        let class = candidate.plan.class();
        let global_limit = self.global_limit(class);
        let global_running = self
            .running
            .iter()
            .filter(|running| {
                self.jobs
                    .get(running)
                    .is_some_and(|record| record.plan.class() == class)
            })
            .count();
        if global_running >= global_limit {
            return Ok(false);
        }

        let provider = candidate.plan.provider().id();
        let provider_running = self
            .running
            .iter()
            .filter(|running| {
                self.jobs.get(running).is_some_and(|record| {
                    record.plan.class() == class && record.plan.provider().id() == provider
                })
            })
            .count();
        if provider_running >= candidate.plan.provider().limits().for_class(class) {
            return Ok(false);
        }

        Ok(!self.running.iter().any(|running| {
            self.jobs
                .get(running)
                .is_some_and(|record| candidate.plan.conflicts_with(&record.plan))
        }))
    }

    const fn global_limit(&self, class: WorkClass) -> usize {
        match class {
            WorkClass::DataMutation => self.limits.operation_data_mutations(),
            WorkClass::Metadata => self.limits.operation_metadata_jobs(),
            WorkClass::HashOrPreview => self.limits.operation_hash_preview_jobs(),
        }
    }

    fn transition(&mut self, id: JobId, state: JobState) -> Result<(), SchedulerError> {
        self.transition_at(id, state, self.clock.now())
    }

    fn transition_at(
        &mut self,
        id: JobId,
        state: JobState,
        occurred_at: u64,
    ) -> Result<(), SchedulerError> {
        let event = {
            let record = self
                .jobs
                .get_mut(&id)
                .ok_or(SchedulerError::UnknownJob(id))?;
            let event = JobEvent::transition(id, record.state.generation(), occurred_at, state);
            record.state.apply(event.clone())?;
            event
        };
        self.events.push(event);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    UnknownJob(JobId),
    NotRunning(JobId),
    JobIdExhausted,
    State(StateError),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownJob(id) => write!(formatter, "unknown job {}", id.get()),
            Self::NotRunning(id) => write!(formatter, "job {} is not running", id.get()),
            Self::JobIdExhausted => formatter.write_str("job identifier space is exhausted"),
            Self::State(error) => error.fmt(formatter),
        }
    }
}

impl Error for SchedulerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            Self::UnknownJob(_) | Self::NotRunning(_) | Self::JobIdExhausted => None,
        }
    }
}

impl From<StateError> for SchedulerError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
