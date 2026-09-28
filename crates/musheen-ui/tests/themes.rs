use musheen_desktop::{SettingsDocument, SettingsStore};
use musheen_ui::settings::{SettingsBackends, SettingsState};
use musheen_ui::theme::document::{ThemeDocument, ThemeError};

fn palette() -> &'static str {
    r##"{"version":1,"tokens":{"background":"#ffffff","foreground":"#111111","primary":"#222222","primary_foreground":"#ffffff","secondary":"#eeeeee","secondary_foreground":"#111111","muted_foreground":"#444444","border":"#555555","ring":"#777777","danger":"#330000","danger_foreground":"#ffffff","warning":"#332200","warning_foreground":"#ffffff"}}"##
}

#[test]
fn semantic_theme_round_trips_and_rejects_missing_unknown_or_invisible_tokens() {
    let theme = ThemeDocument::import(palette()).unwrap();
    assert_eq!(ThemeDocument::import(&theme.export()).unwrap(), theme);
    for invalid in [
        palette().replace("\"ring\":\"#777777\"", "\"icons\":\"lucide\""),
        palette().replace("\"ring\":\"#777777\"", "\"ring\":\"#222222\""),
        palette().replace("#777777", "#ffffff"),
        palette().replace("#111111", "#eeeeee"),
        palette().replace("#777777", "#00000000"),
        palette().replace("\"version\":1", "\"version\":999"),
        "{}".to_owned(),
    ] {
        assert!(ThemeDocument::import(&invalid).is_err(), "{invalid}");
    }
    assert_eq!(
        ThemeDocument::import("{}"),
        Err(ThemeError::InvalidDocument)
    );
}

#[gpui_kit::test]
async fn cancel_restores_both_native_variants_after_cross_mode_preview(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
    use musheen_ui::theme::preview::{AppearanceSnapshot, preview};

    cx.update(|cx| {
        gpui_kit::init(cx);
        let preferences = native_theme::AccessibilityPreferences::default();
        let (light_theme, light) =
            native_theme_gpui::from_preset("kde-breeze", false, &preferences).unwrap();
        let (dark_theme, dark) =
            native_theme_gpui::from_preset("kde-breeze", true, &preferences).unwrap();

        // Install both variants, then return to the native light variant before
        // opening Settings. This matches apply_system_theme's bridge state.
        native_theme_gpui::apply(dark_theme, &dark, &preferences, cx);
        native_theme_gpui::apply(light_theme, &light, &preferences, cx);
        let snapshot = AppearanceSnapshot::capture(cx);

        let mut document = SettingsDocument::default();
        document.set_value("appearance.mode", "dark").unwrap();
        preview(&document, cx);
        assert_eq!(
            cx.global::<native_theme_gpui::NativeTheme>()
                .resolved(cx)
                .unwrap(),
            &dark,
            "explicit dark must select the native dark variant",
        );

        snapshot.restore(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        assert_eq!(
            cx.global::<native_theme_gpui::NativeTheme>()
                .resolved(cx)
                .unwrap(),
            &dark,
            "Cancel must restore the native variant that was not active when Settings opened",
        );
        assert!(cx.theme().mode.is_dark());
    });
}

#[test]
fn import_failure_keeps_preview_and_disk_then_cancel_restores_native() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::at(root.path().join("settings.conf"));
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    state.edit("appearance.theme", palette()).unwrap();
    let valid = state.draft().clone();
    assert!(state.edit("appearance.theme", "{}").is_err());
    assert_eq!(state.draft(), &valid);
    assert!(state.apply(&store).is_err());
    assert!(!store.path().exists());
    state.cancel();
    assert_eq!(
        state.draft().value("appearance.theme").as_deref(),
        Some("native")
    );
    state.edit("appearance.theme", palette()).unwrap();
    state.apply(&store).unwrap();
    assert_eq!(store.load().unwrap(), *state.draft());
    let persisted = std::fs::read(store.path()).unwrap();
    assert!(state.edit("appearance.theme", "{}").is_err());
    assert!(state.errors().contains("appearance.theme"));
    assert!(state.apply(&store).is_err());
    assert_eq!(std::fs::read(store.path()).unwrap(), persisted);
    // A corrected document clears the error and can be committed normally.
    state.edit("appearance.theme", palette()).unwrap();
    assert!(state.errors().is_empty());
    state.apply(&store).unwrap();
    state.edit("appearance.mode", "dark").unwrap();
    state.edit("appearance.reduce_motion", "true").unwrap();
    state.reset_page(musheen_desktop::SettingsPage::Appearance);
    assert_eq!(
        state.draft().value("appearance.theme").as_deref(),
        Some("native")
    );
    assert_eq!(
        state.draft().value("appearance.mode").as_deref(),
        Some("system")
    );
    assert_eq!(
        state.draft().value("appearance.reduce_motion").as_deref(),
        Some("false")
    );
}

#[test]
fn theme_errors_are_localized_and_unknown_fields_cannot_change_icons_or_focus() {
    for locale in [
        musheen_ui::Locale::EnUs,
        musheen_ui::Locale::EnXa,
        musheen_ui::Locale::Ar,
    ] {
        let catalog = musheen_ui::Catalog::load(locale).unwrap();
        for error in [
            ThemeError::InvalidDocument,
            ThemeError::UnsupportedVersion,
            ThemeError::InvalidColor,
            ThemeError::InsufficientContrast,
        ] {
            assert!(!catalog.message(error.message_key()).unwrap().is_empty());
        }
    }
    for field in [
        "icon_family",
        "focus_visible",
        "focus_width",
        "disable_focus",
    ] {
        let invalid = palette().replacen("{", &format!("{{\"{field}\":false,"), 1);
        assert_eq!(
            ThemeDocument::import(&invalid),
            Err(ThemeError::InvalidDocument)
        );
    }
}

#[gpui_kit::test]
async fn runtime_modes_and_custom_tokens_preserve_native_accessibility_and_cancel(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::ActiveTheme;
    use musheen_ui::theme::preview::{AppearanceSnapshot, preview};
    cx.update(|cx| {
        gpui_kit::init(cx);
        let prefs = native_theme::AccessibilityPreferences {
            reduce_motion: true,
            ..Default::default()
        };
        let (dark_theme, dark_resolved) =
            native_theme_gpui::from_preset("kde-breeze", true, &prefs).unwrap();
        native_theme_gpui::apply(dark_theme, &dark_resolved, &prefs, cx);
        let (theme, resolved) =
            native_theme_gpui::from_preset("kde-breeze", false, &prefs).unwrap();
        native_theme_gpui::apply(theme, &resolved, &prefs, cx);
        let native = cx.theme().colors;
        let snapshot = AppearanceSnapshot::capture(cx);
        let mut document = SettingsDocument::default();
        preview(&document, cx);
        assert_eq!(cx.theme().colors.primary, native.primary);
        for (mode, dark, high_contrast) in [
            ("light", false, false),
            ("dark", true, false),
            ("high-contrast", false, true),
        ] {
            document.set_value("appearance.mode", mode).unwrap();
            preview(&document, cx);
            assert_eq!(cx.theme().mode.is_dark(), dark);
            let accessibility = cx
                .global::<native_theme_gpui::NativeTheme>()
                .accessibility();
            assert_eq!(accessibility.high_contrast, high_contrast);
            assert!(accessibility.reduce_motion);
        }
        document.set_value("appearance.mode", "system").unwrap();
        document.set_value("appearance.theme", palette()).unwrap();
        preview(&document, cx);
        let c = cx.theme().colors;
        let rgb = |rgb| gpui_kit::Hsla::from(gpui_kit::rgb(rgb));
        assert_eq!(c.background, rgb(0xffffff));
        assert_eq!(c.button_primary, rgb(0x222222));
        assert_eq!(c.button_primary_hover, c.primary);
        assert_eq!(c.button_primary_foreground, c.primary_foreground);
        assert_eq!(c.list_hover, c.secondary);
        assert_eq!(c.selection, c.secondary);
        assert_eq!(c.input, c.border);
        assert_eq!(c.caret, c.foreground);
        assert_eq!(c.danger, rgb(0x330000));
        assert_eq!(c.button_danger, c.danger);
        assert_eq!(c.warning, rgb(0x332200));
        assert_eq!(c.button_warning, c.warning);
        assert_eq!(c.title_bar, c.background);
        assert_eq!(c.status_bar, c.background);
        assert_eq!(c.sidebar, c.background);
        assert_eq!(gpui_kit::base::Theme::global(cx).tokens.colors.ring, c.ring);
        assert_eq!(
            gpui_kit::base::Theme::global(cx).tokens.colors.background,
            c.background
        );
        assert_eq!(
            gpui_kit::base::Theme::global(cx).resizable.handle,
            Some(c.border)
        );
        assert_eq!(
            gpui_kit::base::Theme::global(cx).resizable.active_handle,
            Some(c.ring)
        );
        snapshot.restore(cx);
        assert_eq!(cx.theme().colors.primary, native.primary);
        assert_eq!(cx.theme().colors.background, native.background);
        assert_eq!(
            cx.global::<native_theme_gpui::NativeTheme>()
                .resolved(cx)
                .unwrap()
                .button
                .primary_background,
            resolved.button.primary_background
        );
        assert!(
            cx.global::<native_theme_gpui::NativeTheme>()
                .accessibility()
                .reduce_motion
        );
    });
}

#[test]
fn multiline_import_is_canonicalized_before_settings_persistence() {
    let pretty = serde_json::to_string_pretty(&ThemeDocument::import(palette()).unwrap()).unwrap();
    assert!(pretty.contains('\n'));
    let mut settings = SettingsDocument::default();
    settings.set_value("appearance.theme", &pretty).unwrap();
    assert!(!settings.value("appearance.theme").unwrap().contains('\n'));
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::at(root.path().join("settings.conf"));
    store.save(&settings).unwrap();
    assert_eq!(store.load().unwrap(), settings);
}

#[gpui_kit::test]
async fn rendered_editor_previews_exports_rejects_invalid_import_and_cancels(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::{ActiveTheme, Root};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, px, size};
    use musheen_ui::{Catalog, Locale};
    let native = cx.update(|cx| {
        gpui_kit::init(cx);
        let prefs = native_theme::AccessibilityPreferences::default();
        let (theme, resolved) = native_theme_gpui::from_preset("adwaita", true, &prefs).unwrap();
        native_theme_gpui::apply(theme, &resolved, &prefs, cx);
        cx.theme().colors
    });
    let root = tempfile::tempdir().unwrap();
    let mut view = None;
    let handle = cx.open_window(size(px(840.), px(980.)), |window, cx| {
        let settings = cx.new(|cx| {
            musheen_ui::settings::SettingsWindow::new(
                SettingsStore::from_config_home(root.path()),
                SettingsBackends::all(),
                Catalog::load(Locale::EnUs).unwrap(),
                window,
                cx,
            )
        });
        view = Some(settings.clone());
        Root::new(settings, window, cx)
    });
    let view = view.unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-page-appearance", cx);
        window.render_frame(cx);
        window.click("theme-starter", cx);
        window.render_frame(cx);
        window.click("theme-preview", cx);
        window.render_frame(cx);
        assert_eq!(
            cx.theme().colors.background,
            gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff))
        );
        let previewed = cx.theme().colors;
        window.click("theme-export", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            ThemeDocument::starter().export()
        );
        window.click("appearance.theme", cx);
        window.press("ctrl-a", cx);
        window.input("{}", cx);
        window.click("theme-preview", cx);
        window.render_frame(cx);
        assert!(view.read(cx).state().errors().contains("appearance.theme"));
        assert_eq!(cx.theme().colors.background, previewed.background);
        assert!(window.find("theme-error").visible());
        window.click("settings-cancel", cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(cx.theme().colors.background, native.background);
        assert_eq!(cx.theme().colors.primary, native.primary);
    });
}

#[gpui_kit::test]
async fn native_high_contrast_wins_over_custom_palette_and_explicit_mode(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::ActiveTheme;
    cx.update(|cx| {
        gpui_kit::init(cx);
        let prefs = native_theme::AccessibilityPreferences {
            high_contrast: true,
            reduce_transparency: true,
            ..Default::default()
        };
        let (theme, resolved) = native_theme_gpui::from_preset("adwaita", false, &prefs).unwrap();
        native_theme_gpui::apply(theme, &resolved, &prefs, cx);
        let mut document = SettingsDocument::default();
        document.set_value("appearance.mode", "dark").unwrap();
        document.set_value("appearance.theme", palette()).unwrap();
        musheen_ui::theme::preview::preview(&document, cx);
        assert!(
            cx.global::<native_theme_gpui::NativeTheme>()
                .accessibility()
                .high_contrast
        );
        assert!(
            cx.global::<native_theme_gpui::NativeTheme>()
                .accessibility()
                .reduce_transparency
        );
        // The imported white surface is bypassed; dark native high contrast wins.
        assert_ne!(
            cx.theme().colors.background,
            gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff))
        );
        let c = cx.theme().colors;
        let ratio = |a: gpui_kit::Hsla, b: gpui_kit::Hsla| {
            let to_hex = |v: gpui_kit::Hsla| {
                let v: gpui_kit::Rgba = v.into();
                format!(
                    "#{:02x}{:02x}{:02x}",
                    (v.r * 255.).round() as u8,
                    (v.g * 255.).round() as u8,
                    (v.b * 255.).round() as u8
                )
            };
            musheen_ui::theme::validate::contrast_ratio(&to_hex(a), &to_hex(b)).unwrap()
        };
        assert!(ratio(c.foreground, c.background) >= 4.5);
        assert!(
            ratio(c.primary_foreground, c.primary) >= 4.5,
            "primary pair"
        );
        assert!(
            ratio(c.secondary_foreground, c.secondary) >= 4.5,
            "secondary pair"
        );
        assert!(ratio(c.muted_foreground, c.background) >= 4.5, "muted text");
        assert!(ratio(c.ring, c.background) >= 3.0);
        assert!(ratio(c.ring, c.secondary) >= 3.0, "secondary focus");
        assert!(ratio(c.border, c.background) >= 3.0);
        assert!(cx.theme().focus_ring);
    });
}

#[gpui_kit::test]
async fn missing_native_extraction_uses_initialized_fallback_and_restores_it(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::ActiveTheme;
    cx.update(|cx| {
        gpui_kit::init(cx);
        assert!(cx.try_global::<native_theme_gpui::NativeTheme>().is_none());
        let original = cx.theme().colors;
        let snapshot = musheen_ui::theme::preview::AppearanceSnapshot::capture(cx);
        let mut document = SettingsDocument::default();
        document.set_value("appearance.theme", palette()).unwrap();
        musheen_ui::theme::preview::preview(&document, cx);
        snapshot.restore(cx);
        assert_eq!(cx.theme().colors.background, original.background);
        assert_eq!(cx.theme().colors.primary, original.primary);
    });
}
