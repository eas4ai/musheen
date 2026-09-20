use musheen_desktop::{SettingSpec, SettingsPage};
pub(super) fn controls() -> Vec<&'static SettingSpec> {
    super::controls_for(SettingsPage::Integrations)
        .into_iter()
        .filter(|spec| spec.group != "settings-group-terminal")
        .collect()
}
