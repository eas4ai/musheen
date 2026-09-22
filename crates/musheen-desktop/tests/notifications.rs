mod support;

use musheen_core::CommandId;
use musheen_desktop::{
    NotificationAction, NotificationEvent, NotificationPolicy, NotificationSink,
    OperationVisibility,
};
use musheen_ops::JobId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use support::UsableFileManager;

#[derive(Default)]
struct RecordingNotifications(Mutex<Vec<NotificationEvent>>);

impl NotificationSink for RecordingNotifications {
    fn send(
        &self,
        event: &NotificationEvent,
        _action_label: &str,
        _actions: async_channel::Sender<NotificationAction>,
    ) -> Result<(), Box<str>> {
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
    let (actions, _receiver) = async_channel::bounded(4);

    policy
        .publish(
            NotificationEvent::completed(job, command.clone(), "Copy complete"),
            OperationVisibility::VisibleWindow,
            "Show in Musheen",
            actions.clone(),
        )
        .unwrap();
    policy
        .publish(
            NotificationEvent::progress(job, command.clone(), "Halfway"),
            OperationVisibility::NoVisibleWindow,
            "Show in Musheen",
            actions.clone(),
        )
        .unwrap();
    policy
        .publish(
            NotificationEvent::failed(job, command, "Copy failed"),
            OperationVisibility::NoVisibleWindow,
            "Show in Musheen",
            actions.clone(),
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
            "Show in Musheen",
            actions.clone(),
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
            "Show in Musheen",
            actions,
        )
        .unwrap();

    let sent = policy.sink().0.lock().unwrap();
    assert_eq!(sent.len(), 3);
    let action = NotificationAction::parse(&sent[0].action_id()).unwrap();
    assert_eq!(action.command_id().as_str(), "file.copy");
    assert_eq!(action.job_id(), job);
    assert!(NotificationAction::parse("foreign.action").is_none());
}

#[derive(Clone, Copy)]
enum NotificationServiceMode {
    Absent,
    Slow,
    Disconnected,
    Restarted,
}

struct MatrixNotifications {
    mode: Mutex<NotificationServiceMode>,
    started: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    delivered: AtomicUsize,
}

impl NotificationSink for MatrixNotifications {
    fn send(
        &self,
        _event: &NotificationEvent,
        _action_label: &str,
        _actions: async_channel::Sender<NotificationAction>,
    ) -> Result<(), Box<str>> {
        match *self.mode.lock().unwrap() {
            NotificationServiceMode::Absent => Err("absent".into()),
            NotificationServiceMode::Disconnected => Err("disconnected".into()),
            NotificationServiceMode::Slow => {
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                Err("slow".into())
            }
            NotificationServiceMode::Restarted => {
                self.delivered.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
    }
}

#[test]
fn notification_absence_slowness_disconnect_and_restart_leave_file_management_usable() {
    let (started, started_rx) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let sink = Arc::new(MatrixNotifications {
        mode: Mutex::new(NotificationServiceMode::Absent),
        started,
        release: Mutex::new(release_rx),
        delivered: AtomicUsize::new(0),
    });
    let file_manager = UsableFileManager::new();
    let (actions, _receiver) = async_channel::bounded(4);
    let event = || {
        NotificationEvent::completed(
            JobId::new(7).unwrap(),
            CommandId::new("file.copy").unwrap(),
            "complete",
        )
    };

    for mode in [
        NotificationServiceMode::Absent,
        NotificationServiceMode::Disconnected,
    ] {
        *sink.mode.lock().unwrap() = mode;
        assert!(
            NotificationPolicy::new(Arc::clone(&sink))
                .publish(
                    event(),
                    OperationVisibility::NoVisibleWindow,
                    "Show",
                    actions.clone(),
                )
                .is_err()
        );
        file_manager.show("usable");
    }

    *sink.mode.lock().unwrap() = NotificationServiceMode::Slow;
    let slow_sink = Arc::clone(&sink);
    let slow_actions = actions.clone();
    let slow = std::thread::spawn(move || {
        NotificationPolicy::new(slow_sink).publish(
            event(),
            OperationVisibility::NoVisibleWindow,
            "Show",
            slow_actions,
        )
    });
    started_rx.recv().unwrap();
    file_manager.show("still-usable");
    release.send(()).unwrap();
    assert!(slow.join().unwrap().is_err());

    *sink.mode.lock().unwrap() = NotificationServiceMode::Restarted;
    NotificationPolicy::new(Arc::clone(&sink))
        .publish(
            event(),
            OperationVisibility::NoVisibleWindow,
            "Show",
            actions,
        )
        .unwrap();
    file_manager.show("restarted");
    assert_eq!(sink.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(file_manager.calls(), 4);
}
