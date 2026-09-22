use super::{SettingKind, SettingSpec, SettingsError};

pub(super) fn validate_value(spec: &SettingSpec, value: &str) -> Result<(), SettingsError> {
    let valid = match spec.kind {
        SettingKind::Theme => super::theme::validate_setting(value).is_ok(),
        SettingKind::CustomActions => crate::CustomActionDocument::import(value).is_ok(),
        SettingKind::Toolbar => musheen_core::ToolbarLayout::import(value).is_ok(),
        SettingKind::Shortcuts => musheen_core::ShortcutMap::import(value).is_ok(),
        SettingKind::Boolean => matches!(value, "true" | "false"),
        SettingKind::Choice(choices) => choices.contains(&value),
        SettingKind::Integer { maximum, .. } => value
            .parse::<usize>()
            .is_ok_and(|value| (1..=maximum).contains(&value)),
        SettingKind::CredentialReference => {
            value.is_empty() || crate::CredentialReference::from_setting_value(value).is_ok()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(SettingsError::InvalidValue {
            key: spec.key.into(),
        })
    }
}
