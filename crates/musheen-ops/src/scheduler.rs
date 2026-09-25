use crate::{
    ArchiveEventPhase, ArchiveOperationPlan, EventGeneration, JobEvent, JobId, JobState,
    JobStateMachine, OperationPlan, PlanError, ProviderSnapshot, StateError, WorkClass,
};
use musheen_core::{CancellationToken, ResourceLimits};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};
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
    pub fn archive_plan(&self) -> Option<&ArchiveOperationPlan> {
        self.plan.archive()
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
    committing: bool,
}

#[derive(Debug)]
struct SchedulerData<C> {
    clock: C,
    next_job_id: u64,
    queued: VecDeque<JobId>,
    running: BTreeSet<JobId>,
    jobs: BTreeMap<JobId, JobRecord>,
    events: Vec<JobEvent>,
}

#[derive(Debug)]
pub struct Scheduler<C = SystemClock> {
    limits: ResourceLimits,
    data: Arc<Mutex<SchedulerData<C>>>,
}

impl<C> Clone for Scheduler<C> {
    fn clone(&self) -> Self {
        Self {
            limits: self.limits.clone(),
            data: Arc::clone(&self.data),
        }
    }
}

impl Scheduler<SystemClock> {
    #[must_use]
    pub fn new(limits: &ResourceLimits) -> Self {
        Self::with_clock(limits, SystemClock::default())
    }

    pub fn new_starting_after(
        limits: &ResourceLimits,
        last_job_id: JobId,
    ) -> Result<Self, SchedulerError> {
        Self::with_clock_starting_after(limits, SystemClock::default(), last_job_id)
    }
}

impl<C: Clock> Scheduler<C> {
    #[must_use]
    pub fn with_clock(limits: &ResourceLimits, clock: C) -> Self {
        Self::with_next_job_id(limits, clock, 1)
    }

    pub fn with_clock_starting_after(
        limits: &ResourceLimits,
        clock: C,
        last_job_id: JobId,
    ) -> Result<Self, SchedulerError> {
        let next_job_id = last_job_id
            .get()
            .checked_add(1)
            .ok_or(SchedulerError::JobIdExhausted)?;
        Ok(Self::with_next_job_id(limits, clock, next_job_id))
    }

    fn with_next_job_id(limits: &ResourceLimits, clock: C, next_job_id: u64) -> Self {
        Self {
            limits: limits.snapshot(),
            data: Arc::new(Mutex::new(SchedulerData {
                clock,
                next_job_id,
                queued: VecDeque::new(),
                running: BTreeSet::new(),
                jobs: BTreeMap::new(),
                events: Vec::new(),
            })),
        }
    }

    pub fn enqueue(&self, plan: OperationPlan) -> Result<JobId, SchedulerError> {
        let mut data = self.lock();
        let id = JobId::new(data.next_job_id).ok_or(SchedulerError::JobIdExhausted)?;
        data.next_job_id = data
            .next_job_id
            .checked_add(1)
            .ok_or(SchedulerError::JobIdExhausted)?;
        let cancellation = CancellationToken::new();
        let state = JobStateMachine::new(id);
        data.jobs.insert(
            id,
            JobRecord {
                plan,
                state,
                cancellation,
                committing: false,
            },
        );
        transition(&mut data, id, JobState::Queued)?;
        data.queued.push_back(id);
        Ok(id)
    }

    pub fn enqueue_archive(
        &self,
        plan: ArchiveOperationPlan,
        provider: ProviderSnapshot,
    ) -> Result<JobId, SchedulerError> {
        self.enqueue(plan.into_operation_plan(provider)?)
    }

    pub fn start_ready(&self) -> Result<Vec<ScheduledJob>, SchedulerError> {
        let mut data = self.lock();
        let queued = data.queued.len();
        let mut started = Vec::new();
        for _ in 0..queued {
            let id = data
                .queued
                .pop_front()
                .expect("the recorded queue length remains exact");
            if !can_start(&data, &self.limits, id)? {
                data.queued.push_back(id);
                continue;
            }
            let started_at = data.clock.now();
            transition_at(&mut data, id, JobState::Running, started_at)?;
            data.running.insert(id);
            let record = data.jobs.get(&id).ok_or(SchedulerError::UnknownJob(id))?;
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

    pub fn complete(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        transition(&mut data, id, JobState::Completed)?;
        data.jobs
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .committing = false;
        data.running.remove(&id);
        Ok(())
    }

    pub fn fail(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        transition(&mut data, id, JobState::Failed)?;
        data.jobs
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .committing = false;
        data.running.remove(&id);
        Ok(())
    }

    pub fn pause(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        if data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .committing
        {
            return Err(SchedulerError::CommitInProgress(id));
        }
        let cancellation = data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .cancellation
            .clone();
        transition(&mut data, id, JobState::Paused)?;
        cancellation.pause();
        Ok(())
    }

    pub fn resume(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        let cancellation = data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .cancellation
            .clone();
        transition(&mut data, id, JobState::Running)?;
        cancellation.resume();
        Ok(())
    }

    pub fn cancel(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .committing
        {
            return Err(SchedulerError::CommitInProgress(id));
        }
        let state = data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .state
            .state();
        let cancellation = data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .cancellation
            .clone();
        cancellation.cancel();
        if state == JobState::Queued {
            data.queued.retain(|queued| *queued != id);
            transition(&mut data, id, JobState::Cancelled)?;
        } else {
            transition(&mut data, id, JobState::Cancelling)?;
        }
        Ok(())
    }

    pub fn finish_cancel(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        transition(&mut data, id, JobState::Cancelled)?;
        data.running.remove(&id);
        Ok(())
    }

    pub fn retry(&self, id: JobId) -> Result<EventGeneration, SchedulerError> {
        let mut data = self.lock();
        let now = data.clock.now();
        let generation = data
            .jobs
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .state
            .retry(now)?;
        let record = data
            .jobs
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownJob(id))?;
        record.cancellation = CancellationToken::new();
        record.committing = false;
        data.queued.push_back(id);
        Ok(generation)
    }

    pub fn interrupt(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        if data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .committing
        {
            return Err(SchedulerError::CommitInProgress(id));
        }
        let cancellation = data
            .jobs
            .get(&id)
            .ok_or(SchedulerError::UnknownJob(id))?
            .cancellation
            .clone();
        cancellation.cancel();
        transition(&mut data, id, JobState::Interrupted)?;
        data.running.remove(&id);
        Ok(())
    }

    #[must_use]
    pub fn state(&self, id: JobId) -> Option<JobState> {
        self.lock().jobs.get(&id).map(|record| record.state.state())
    }

    #[must_use]
    pub const fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    #[must_use]
    pub fn events(&self) -> Vec<JobEvent> {
        self.lock().events.clone()
    }

    /// Atomically admits a running job into its non-cancellable publication section.
    ///
    /// Once admitted, pause, cancellation, and interruption are rejected until the executor
    /// reports completion or failure. This keeps publication and the terminal scheduler state one
    /// indivisible operation from the point of view of control callers.
    pub fn begin_commit(&self, id: JobId) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        if !data.running.contains(&id) {
            return Err(SchedulerError::NotRunning(id));
        }
        let record = data
            .jobs
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownJob(id))?;
        if record.state.state() != JobState::Running || record.cancellation.is_cancelled() {
            return Err(SchedulerError::CommitAdmissionDenied(id));
        }
        record.committing = true;
        Ok(())
    }

    /// Publishes an archive phase through the same ordered event stream as state changes.
    pub fn emit_archive_phase(
        &self,
        id: JobId,
        phase: ArchiveEventPhase,
    ) -> Result<(), SchedulerError> {
        let mut data = self.lock();
        let occurred_at = data.clock.now();
        let event = {
            let record = data
                .jobs
                .get_mut(&id)
                .ok_or(SchedulerError::UnknownJob(id))?;
            let event = JobEvent::archive_phase(id, record.state.generation(), occurred_at, phase);
            record.state.apply(event.clone())?;
            event
        };
        data.events.push(event);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SchedulerData<C>> {
        self.data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn can_start<C: Clock>(
    data: &SchedulerData<C>,
    limits: &ResourceLimits,
    id: JobId,
) -> Result<bool, SchedulerError> {
    let candidate = data.jobs.get(&id).ok_or(SchedulerError::UnknownJob(id))?;
    let class = candidate.plan.class();
    let global_limit = match class {
        WorkClass::DataMutation => limits.operation_data_mutations(),
        WorkClass::Metadata => limits.operation_metadata_jobs(),
        WorkClass::HashOrPreview => limits.operation_hash_preview_jobs(),
    };
    let global_running = data
        .running
        .iter()
        .filter(|running| {
            data.jobs
                .get(running)
                .is_some_and(|record| record.plan.class() == class)
        })
        .count();
    if global_running >= global_limit {
        return Ok(false);
    }

    let provider = candidate.plan.provider().id();
    let provider_running = data
        .running
        .iter()
        .filter(|running| {
            data.jobs.get(running).is_some_and(|record| {
                record.plan.class() == class && record.plan.provider().id() == provider
            })
        })
        .count();
    if provider_running >= candidate.plan.provider().limits().for_class(class) {
        return Ok(false);
    }

    Ok(!data.running.iter().any(|running| {
        data.jobs
            .get(running)
            .is_some_and(|record| candidate.plan.conflicts_with(&record.plan))
    }))
}

fn transition<C: Clock>(
    data: &mut SchedulerData<C>,
    id: JobId,
    state: JobState,
) -> Result<(), SchedulerError> {
    let occurred_at = data.clock.now();
    transition_at(data, id, state, occurred_at)
}

fn transition_at<C: Clock>(
    data: &mut SchedulerData<C>,
    id: JobId,
    state: JobState,
    occurred_at: u64,
) -> Result<(), SchedulerError> {
    let event = {
        let record = data
            .jobs
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownJob(id))?;
        let event = JobEvent::transition(id, record.state.generation(), occurred_at, state);
        record.state.apply(event.clone())?;
        event
    };
    data.events.push(event);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    UnknownJob(JobId),
    NotRunning(JobId),
    CommitInProgress(JobId),
    CommitAdmissionDenied(JobId),
    JobIdExhausted,
    State(StateError),
    InvalidPlan(PlanError),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownJob(id) => write!(formatter, "unknown job {}", id.get()),
            Self::NotRunning(id) => write!(formatter, "job {} is not running", id.get()),
            Self::CommitInProgress(id) => {
                write!(
                    formatter,
                    "job {} is committing and cannot be controlled",
                    id.get()
                )
            }
            Self::CommitAdmissionDenied(id) => {
                write!(
                    formatter,
                    "job {} cannot enter its commit section",
                    id.get()
                )
            }
            Self::JobIdExhausted => formatter.write_str("job identifier space is exhausted"),
            Self::State(error) => error.fmt(formatter),
            Self::InvalidPlan(error) => error.fmt(formatter),
        }
    }
}

impl Error for SchedulerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            Self::InvalidPlan(error) => Some(error),
            Self::UnknownJob(_)
            | Self::NotRunning(_)
            | Self::CommitInProgress(_)
            | Self::CommitAdmissionDenied(_)
            | Self::JobIdExhausted => None,
        }
    }
}

impl From<StateError> for SchedulerError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}

impl From<PlanError> for SchedulerError {
    fn from(error: PlanError) -> Self {
        Self::InvalidPlan(error)
    }
}
