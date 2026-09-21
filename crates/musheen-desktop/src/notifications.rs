use musheen_core::CommandId;
use musheen_ops::JobId;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationVisibility {
    VisibleWindow,
    NoVisibleWindow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationOutcome {
    Progress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationEvent {
    job_id: JobId,
    command_id: CommandId,
    outcome: NotificationOutcome,
    summary: Box<str>,
}

impl NotificationEvent {
    #[must_use]
    pub fn progress(job_id: JobId, command_id: CommandId, summary: impl Into<Box<str>>) -> Self {
        Self::new(job_id, command_id, NotificationOutcome::Progress, summary)
    }

    #[must_use]
    pub fn completed(job_id: JobId, command_id: CommandId, summary: impl Into<Box<str>>) -> Self {
        Self::new(job_id, command_id, NotificationOutcome::Completed, summary)
    }

    #[must_use]
    pub fn failed(job_id: JobId, command_id: CommandId, summary: impl Into<Box<str>>) -> Self {
        Self::new(job_id, command_id, NotificationOutcome::Failed, summary)
    }

    fn new(
        job_id: JobId,
        command_id: CommandId,
        outcome: NotificationOutcome,
        summary: impl Into<Box<str>>,
    ) -> Self {
        Self {
            job_id,
            command_id,
            outcome,
            summary: summary.into(),
        }
    }

    #[must_use]
    pub fn action_id(&self) -> String {
        format!(
            "musheen.command.{}.job.{}",
            self.command_id.as_str(),
            self.job_id.get()
        )
    }

    #[must_use]
    pub const fn job_id(&self) -> JobId {
        self.job_id
    }

    #[must_use]
    pub const fn command_id(&self) -> &CommandId {
        &self.command_id
    }

    #[must_use]
    pub const fn outcome(&self) -> NotificationOutcome {
        self.outcome
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationAction {
    command_id: CommandId,
    job_id: JobId,
}

impl NotificationAction {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.strip_prefix("musheen.command.")?;
        let (command, job) = value.rsplit_once(".job.")?;
        Some(Self {
            command_id: CommandId::new(command).ok()?,
            job_id: JobId::new(job.parse().ok()?)?,
        })
    }

    #[must_use]
    pub const fn command_id(&self) -> &CommandId {
        &self.command_id
    }

    #[must_use]
    pub const fn job_id(&self) -> JobId {
        self.job_id
    }
}

pub trait NotificationSink: Send + Sync + 'static {
    fn send(&self, event: &NotificationEvent) -> Result<(), Box<str>>;
}

impl<T: NotificationSink + ?Sized> NotificationSink for Arc<T> {
    fn send(&self, event: &NotificationEvent) -> Result<(), Box<str>> {
        (**self).send(event)
    }
}

pub struct NotificationPolicy<S> {
    sink: S,
}

impl<S: NotificationSink> NotificationPolicy<S> {
    #[must_use]
    pub const fn new(sink: S) -> Self {
        Self { sink }
    }

    pub fn publish(
        &self,
        event: NotificationEvent,
        visibility: OperationVisibility,
    ) -> Result<(), NotificationError> {
        if visibility == OperationVisibility::NoVisibleWindow
            && matches!(
                event.outcome,
                NotificationOutcome::Completed | NotificationOutcome::Failed
            )
        {
            self.sink.send(&event).map_err(NotificationError::Backend)?;
        }
        Ok(())
    }

    #[must_use]
    pub const fn sink(&self) -> &S {
        &self.sink
    }
}

#[derive(Debug)]
pub enum NotificationError {
    Backend(Box<str>),
}

impl fmt::Display for NotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(reason) => write!(formatter, "desktop notification failed: {reason}"),
        }
    }
}

impl Error for NotificationError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct NotifyRustSink;

impl NotificationSink for NotifyRustSink {
    fn send(&self, event: &NotificationEvent) -> Result<(), Box<str>> {
        notify_rust::Notification::new()
            .appname("Musheen")
            .summary(event.summary())
            .action(&event.action_id(), "Show in Musheen")
            .show()
            .map(|_| ())
            .map_err(|error| error.to_string().into())
    }
}
