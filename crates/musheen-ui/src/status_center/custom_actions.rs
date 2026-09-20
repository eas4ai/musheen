use super::{OperationStatus, StatusCenterModel};
use crate::Catalog;
use musheen_core::{DisplayPath, StorePath};
use musheen_desktop::CustomActionError;
use serde::{Deserialize, Serialize};

const MAX_HISTORY: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomActionContext {
    pub action_id: String,
    pub label: String,
    pub targets: Vec<StorePath>,
    pub target_count: usize,
    pub location: StorePath,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomActionStatus {
    pub id: u64,
    pub context: CustomActionContext,
    pub status: OperationStatus,
    cause: Option<String>,
    exit_status: Option<i32>,
    dismissed: bool,
}

impl CustomActionStatus {
    pub fn message(&self, catalog: &Catalog) -> String {
        let label = safe_text(&self.context.label);
        let state = match self.status {
            OperationStatus::Running => "custom-action-job-running",
            OperationStatus::Completed => "custom-action-job-completed",
            _ => "custom-action-job-failed",
        };
        let mut message = format!(
            "{label} — {}",
            catalog.message(state).expect("action status")
        );
        message.push_str(&format!(
            ". {}: {}",
            catalog
                .message("custom-action-job-location")
                .expect("action location"),
            safe_path(&self.context.location)
        ));
        message.push_str(&format!(
            ". {} ({}): {}",
            catalog
                .message("custom-action-job-targets")
                .expect("action targets"),
            self.context.target_count,
            self.context
                .targets
                .iter()
                .map(safe_path)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if let Some(cause) = &self.cause {
            message.push_str(&format!(
                ". {}",
                catalog.message(cause).unwrap_or_else(|_| catalog
                    .message("custom-action-invalid")
                    .expect("action fallback"))
            ));
            if let Some(code) = self.exit_status {
                message.push_str(&format!(" ({code})"));
            }
            message.push_str(&format!(
                ". {}",
                catalog
                    .message("custom-action-job-recovery")
                    .expect("action recovery")
            ));
        }
        message
    }
    pub fn visible(&self) -> bool {
        !self.dismissed
    }
}

fn safe_path(path: &StorePath) -> String {
    safe_text(DisplayPath::from_store_path(path).as_str())
}

pub(crate) fn safe_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

impl StatusCenterModel {
    pub(crate) fn custom_actions(&self) -> &[CustomActionStatus] {
        &self.custom_actions
    }

    pub(crate) fn register_custom_action(
        &mut self,
        context: CustomActionContext,
    ) -> Result<u64, CustomActionError> {
        if self
            .custom_actions
            .iter()
            .filter(|entry| entry.status == OperationStatus::Running)
            .count()
            >= 4
        {
            return Err(CustomActionError::ConcurrencyLimit);
        }
        let id = self
            .custom_actions
            .iter()
            .map(|entry| entry.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(CustomActionError::InvalidDocument)?;
        self.trim_custom_history();
        self.custom_actions.push(CustomActionStatus {
            id,
            context,
            status: OperationStatus::Running,
            cause: None,
            exit_status: None,
            dismissed: false,
        });
        Ok(id)
    }

    pub(crate) fn reject_custom_action(
        &mut self,
        context: CustomActionContext,
        error: &CustomActionError,
    ) {
        self.trim_custom_history();
        let id = self
            .custom_actions
            .iter()
            .map(|entry| entry.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.custom_actions.push(CustomActionStatus {
            id,
            context,
            status: OperationStatus::Failed,
            cause: Some(error.message_key().into()),
            exit_status: None,
            dismissed: false,
        });
    }

    pub(crate) fn finish_custom_action(&mut self, id: u64, result: &Result<(), CustomActionError>) {
        let Some(entry) = self.custom_actions.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        entry.status = if result.is_ok() {
            OperationStatus::Completed
        } else {
            OperationStatus::Failed
        };
        if let Err(error) = result {
            entry.cause = Some(error.message_key().into());
            entry.exit_status = match error {
                CustomActionError::ExitStatus(code) => *code,
                _ => None,
            };
        }
    }

    pub(crate) fn dismiss_custom_action(&mut self, id: u64) {
        if let Some(entry) = self.custom_actions.iter_mut().find(|entry| entry.id == id)
            && entry.status != OperationStatus::Running
        {
            entry.dismissed = true;
        }
    }

    pub(super) fn interrupt_custom_actions(&mut self) -> bool {
        let mut changed = false;
        for entry in &mut self.custom_actions {
            if entry.status == OperationStatus::Running {
                entry.status = OperationStatus::Interrupted;
                entry.cause = Some("custom-action-job-interrupted".into());
                changed = true;
            }
        }
        changed
    }

    fn trim_custom_history(&mut self) {
        if self.custom_actions.len() >= MAX_HISTORY
            && let Some(index) = self
                .custom_actions
                .iter()
                .position(|entry| entry.status != OperationStatus::Running)
        {
            self.custom_actions.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> CustomActionContext {
        CustomActionContext {
            action_id: "test".into(),
            label: "Test".into(),
            targets: vec![StorePath::from_unix_path("/tmp/item")],
            target_count: 1,
            location: StorePath::from_unix_path("/tmp"),
        }
    }

    #[test]
    fn custom_action_history_is_bounded_shared_and_survives_restart() {
        let hub = crate::operations::OperationHub::new(&musheen_core::ResourceLimits::default());
        let second_window = hub.clone();
        let ids = (0..4)
            .map(|_| hub.submit_custom_action(context()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            second_window
                .submit_custom_action(context())
                .unwrap_err()
                .message_key(),
            "custom-action-busy"
        );
        for id in ids {
            second_window.finish_custom_action(id, &Ok(()));
        }
        for _ in 0..140 {
            let id = hub.submit_custom_action(context()).unwrap();
            hub.finish_custom_action(id, &Err(CustomActionError::Timeout));
        }
        let id = hub.submit_custom_action(context()).unwrap();
        let status = hub.status();
        let status = status.lock().unwrap();
        assert_eq!(status.custom_actions().len(), 128);
        let mut restored = StatusCenterModel::from_json(&status.to_json().unwrap()).unwrap();
        assert!(restored.mark_unfinished_interrupted());
        assert_eq!(restored.custom_actions().last().unwrap().id, id);
        assert_eq!(
            restored.custom_actions().last().unwrap().status,
            OperationStatus::Interrupted
        );
        assert_eq!(
            restored
                .custom_actions()
                .iter()
                .filter(|entry| entry.status == OperationStatus::Running)
                .count(),
            0
        );
    }

    #[test]
    fn status_documents_from_before_custom_actions_remain_compatible() {
        let model = StatusCenterModel::default();
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&model.to_json().unwrap()).unwrap();
        legacy
            .as_object_mut()
            .expect("status document is an object")
            .remove("custom_actions");

        let restored = StatusCenterModel::from_json(&serde_json::to_vec(&legacy).unwrap()).unwrap();

        assert!(restored.custom_actions().is_empty());
    }
}
