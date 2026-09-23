use musheen_core::CommandRegistry;
use musheen_ui::{
    ApplicationIdentity, Catalog, ContentIdentity, Locale, LucideIcon, freedesktop_icon_name,
    lucide_icon, lucide_icon_or_fallback,
};

#[test]
fn every_visible_command_has_a_localized_name_and_icon_fallback() {
    let catalog = Catalog::load(Locale::EnUs).expect("the built-in English catalog is valid");

    for command in CommandRegistry::built_in().commands() {
        let label = catalog
            .message(command.label_key())
            .unwrap_or_else(|error| panic!("{}: {error}", command.id().as_str()));
        assert!(
            !label.trim().is_empty(),
            "{} has no name",
            command.id().as_str()
        );
        assert!(
            lucide_icon(command.icon_key()).is_some(),
            "{} has no registered icon or fallback",
            command.id().as_str()
        );
    }
}

#[test]
fn an_unknown_action_icon_uses_the_extension_puzzle_fallback() {
    assert_eq!(lucide_icon("extension-action"), None);
    assert_eq!(
        lucide_icon_or_fallback("extension-action"),
        LucideIcon::Puzzle
    );
}

#[test]
fn command_icon_keys_resolve_to_semantic_lucide_glyphs() {
    for (key, expected) in [
        ("scissors", LucideIcon::Scissors),
        ("terminal", LucideIcon::Terminal),
        ("eye-off", LucideIcon::EyeOff),
        ("eye", LucideIcon::Eye),
        ("file-symlink", LucideIcon::FileSymlink),
        ("link-2", LucideIcon::Link2),
        ("tag", LucideIcon::Tag),
        ("shield", LucideIcon::Shield),
        ("pin", LucideIcon::Pin),
    ] {
        assert_eq!(lucide_icon(key), Some(expected), "{key}");
    }
}

#[test]
fn pseudo_locale_covers_and_expands_every_english_message() {
    let english = Catalog::load(Locale::EnUs).expect("the English catalog is valid");
    let pseudo = Catalog::load(Locale::EnXa).expect("the pseudo catalog is valid");

    assert_eq!(english.message_ids(), pseudo.message_ids());
    for id in english.message_ids() {
        let source = english
            .message(id)
            .expect("the enumerated English message exists");
        let expanded = pseudo.message(id).expect("the pseudo message exists");
        assert!(expanded.starts_with('⟦') && expanded.ends_with('⟧'), "{id}");
        assert!(expanded.chars().count() >= source.chars().count(), "{id}");
    }
}

#[test]
fn application_and_content_icons_keep_distinct_identity_sources() {
    assert_eq!(ApplicationIdentity::ID, "org.musheen.Musheen");
    assert_eq!(ApplicationIdentity::ICON_NAME, "musheen");
    assert!(ApplicationIdentity::ICON_SVG.starts_with(b"<svg"));

    assert_eq!(
        freedesktop_icon_name(&ContentIdentity::directory()),
        "folder"
    );
    assert_eq!(
        freedesktop_icon_name(&ContentIdentity::mime("image/png")),
        "image-png"
    );
    assert_ne!(
        freedesktop_icon_name(&ContentIdentity::directory()),
        ApplicationIdentity::ICON_NAME
    );
}

#[test]
fn linux_desktop_surfaces_have_localized_accessible_names() {
    let message_ids = [
        "terminal-drawer-label",
        "terminal-close",
        "terminal-restart",
        "elevated-browser-warning",
        "volume-unlock-title",
        "volume-properties-dialog",
        "notification-show-in-musheen",
        "dialog-authorization-provider",
    ];
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        let catalog = Catalog::load(locale).expect("the built-in locale catalog is valid");
        for id in message_ids {
            let label = catalog
                .message(id)
                .unwrap_or_else(|error| panic!("{} is missing {id}: {error}", locale.tag()));
            assert!(
                !label.trim().is_empty(),
                "{} has an empty {id}",
                locale.tag()
            );
        }
    }
}
