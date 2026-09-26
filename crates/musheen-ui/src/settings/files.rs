use musheen_desktop::{SettingSpec, SettingsPage};
pub(super) fn controls() -> Vec<&'static SettingSpec> {
    super::controls_for(SettingsPage::Files)
}
