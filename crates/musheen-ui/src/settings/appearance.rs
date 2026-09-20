use crate::{AppearanceMode, MotionPolicy, ThemeProfile};
use musheen_desktop::{SettingSpec, SettingsDocument, SettingsPage};

pub(super) fn controls() -> Vec<&'static SettingSpec> {
    super::controls_for(SettingsPage::Appearance)
}

pub fn appearance_profile(document: &SettingsDocument, native: ThemeProfile) -> ThemeProfile {
    let mode = match document.value("appearance.mode").as_deref() {
        Some("light") => AppearanceMode::Light,
        Some("dark") => AppearanceMode::Dark,
        Some("high-contrast") => AppearanceMode::HighContrast,
        _ => native.mode(),
    };
    ThemeProfile::new(
        mode,
        native.motion() == MotionPolicy::Reduced
            || document.value("appearance.reduce_motion").as_deref() == Some("true"),
    )
}
