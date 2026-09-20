use super::{SettingKind, SettingSpec, SettingsError};

pub(super) fn validate_value(spec: &SettingSpec, value: &str) -> Result<(), SettingsError> {
    let valid = match spec.kind {
        SettingKind::Boolean => matches!(value, "true" | "false"),
        SettingKind::Choice(choices) => choices.contains(&value),
        SettingKind::Integer { maximum, .. } => value
            .parse::<usize>()
            .is_ok_and(|value| (1..=maximum).contains(&value)),
        SettingKind::CredentialReference => {
            value.is_empty()
                || value.strip_prefix("secret-service:").is_some_and(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                })
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
