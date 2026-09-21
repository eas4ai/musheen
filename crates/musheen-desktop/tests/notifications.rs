use musheen_core::CommandId;
use musheen_desktop::{
    NotificationAction, NotificationEvent, NotificationPolicy, NotificationSink,
    OperationVisibility,
};
use musheen_ops::JobId;
use std::sync::Mutex;

#[derive(Default)]
struct RecordingNotifications(Mutex<Vec<NotificationEvent>>);

impl NotificationSink for RecordingNotifications {
    fn send(&self, event: &NotificationEvent) -> Result<(), Box<str>> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[test]
fn only_background_completion_and_failure_notify() {
    let sink = RecordingNotifications::default();
    let policy = NotificationPolicy::new(sink);
    let job = JobId::new(42).unwrap();
    let command = CommandId::new("file.copy").unwrap();

    policy
        .publish(
            NotificationEvent::completed(job, command.clone(), "Copy complete"),
            OperationVisibility::VisibleWindow,
        )
        .unwrap();
    policy
        .publish(
            NotificationEvent::progress(job, command.clone(), "Halfway"),
            OperationVisibility::NoVisibleWindow,
        )
        .unwrap();
    policy
        .publish(
            NotificationEvent::failed(job, command, "Copy failed"),
            OperationVisibility::NoVisibleWindow,
        )
        .unwrap();
    policy
        .publish(
            NotificationEvent::completed(
                JobId::new(43).unwrap(),
                CommandId::new("file.copy").unwrap(),
                "Hidden-window copy complete",
            ),
            OperationVisibility::NoVisibleWindow,
        )
        .unwrap();
    policy
        .publish(
            NotificationEvent::failed(
                JobId::new(44).unwrap(),
                CommandId::new("file.copy").unwrap(),
                "Closed-window copy failed",
            ),
            OperationVisibility::NoVisibleWindow,
        )
        .unwrap();

    let sent = policy.sink().0.lock().unwrap();
    assert_eq!(sent.len(), 3);
    let action = NotificationAction::parse(&sent[0].action_id()).unwrap();
    assert_eq!(action.command_id().as_str(), "file.copy");
    assert_eq!(action.job_id(), job);
    assert!(NotificationAction::parse("foreign.action").is_none());
}
