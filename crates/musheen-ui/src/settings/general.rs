use musheen_desktop::{SettingSpec, SettingsPage};
pub(super) fn controls(page: SettingsPage) -> Vec<&'static SettingSpec> {
    super::controls_for(page)
}

pub(crate) fn configure_navigation(
    navigation: &mut crate::navigation::WindowSession,
    document: &musheen_desktop::SettingsDocument,
) {
    let preferences = view_preferences(document);
    navigation.set_default_preferences(preferences);
    let location = navigation.focused_tab().location().clone();
    if document.value("layout.panes").as_deref() == Some("two") && navigation.panes().len() == 1 {
        // A new second pane is within the session's fixed two-pane capacity.
        navigation
            .split_focused(location)
            .expect("one pane can split");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        navigation::WindowSession,
        views::{Layout, ViewPreferences},
    };
    use musheen_core::StorePath;
    use musheen_desktop::SettingsDocument;

    #[test]
    fn saved_defaults_cover_new_folders_without_replacing_explicit_folder_preferences() {
        let path = StorePath::from_unix_path("/saved");
        let mut navigation = WindowSession::new(path.clone());
        let explicit = ViewPreferences::default();
        navigation.set_preferences_for(path.clone(), explicit.clone());
        let mut settings = SettingsDocument::default();
        settings.set_value("layout.view", "grid").unwrap();
        settings.set_value("files.hidden", "true").unwrap();
        configure_navigation(&mut navigation, &settings);
        assert_eq!(navigation.preferences_for(&path), &explicit);
        assert_eq!(navigation.focused_tab().view_preferences(), &explicit);
        let fresh = navigation.preferences_for(&StorePath::from_unix_path("/fresh"));
        assert_eq!(fresh.layout, Layout::Grid);
        assert!(fresh.show_hidden);
    }
}

pub(crate) fn view_preferences(
    document: &musheen_desktop::SettingsDocument,
) -> crate::views::ViewPreferences {
    use crate::views::{Layout, ViewPreferences};
    ViewPreferences {
        layout: match document.value("layout.view").as_deref() {
            Some("list") => Layout::List,
            Some("cards") => Layout::Cards,
            Some("grid") => Layout::Grid,
            Some("columns") => Layout::Columns,
            Some("adaptive") => Layout::Adaptive,
            _ => Layout::Details,
        },
        show_hidden: document.value("files.hidden").as_deref() == Some("true"),
        directories_first: document.value("files.directories_first").as_deref() != Some("false"),
        ..ViewPreferences::default()
    }
}
