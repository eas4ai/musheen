use crate::{Catalog, Locale};
use musheen_desktop::{SettingKind, SettingSpec};

pub(super) fn choices(kind: SettingKind) -> &'static [&'static str] {
    match kind {
        SettingKind::Boolean => &["false", "true"],
        SettingKind::Choice(values) => values,
        _ => &[],
    }
}

pub(super) fn display_value(spec: &SettingSpec, value: &str, catalog: &Catalog) -> String {
    match spec.kind {
        SettingKind::Boolean | SettingKind::Choice(_) => catalog
            .message(&format!("settings-value-{value}"))
            .expect("schema option is localized")
            .to_owned(),
        SettingKind::Integer { .. } => display_number(value, catalog.locale()),
        SettingKind::CredentialReference if value.is_empty() => catalog
            .message("settings-value-none")
            .expect("empty reference is localized")
            .to_owned(),
        SettingKind::CredentialReference => value.to_owned(),
    }
}

/// Editable references remain opaque identifiers; localized absence belongs in
/// the placeholder, never in a value that might be written to the store.
pub(super) fn input_value(spec: &SettingSpec, value: &str, catalog: &Catalog) -> String {
    if matches!(spec.kind, SettingKind::CredentialReference) {
        value.to_owned()
    } else {
        display_value(spec, value, catalog)
    }
}

pub(super) fn display_number(value: &str, locale: Locale) -> String {
    match locale {
        Locale::Ar => value
            .chars()
            .map(|ch| match ch {
                '0'..='9' => char::from_u32('٠' as u32 + ch as u32 - '0' as u32)
                    .expect("Arabic decimal digit"),
                _ => ch,
            })
            .collect(),
        Locale::EnXa => format!("⟦{value}⟧"),
        Locale::EnUs => value.to_owned(),
    }
}

pub(super) fn stored_number(value: &str) -> String {
    value
        .trim()
        .trim_matches(['⟦', '⟧'])
        .chars()
        .map(|ch| match ch {
            '٠'..='٩' => {
                char::from_u32('0' as u32 + ch as u32 - '٠' as u32).expect("ASCII decimal digit")
            }
            _ => ch,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_choice_labels_and_numeric_defaults_have_localized_presentations() {
        for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
            let catalog = Catalog::load(locale).unwrap();
            for spec in musheen_desktop::settings_schema() {
                let choices = match spec.kind {
                    SettingKind::Boolean => &["true", "false"][..],
                    SettingKind::Choice(choices) => choices,
                    _ => &[],
                };
                for value in choices {
                    assert!(!display_value(spec, value, &catalog).is_empty());
                }
                if let SettingKind::Integer { .. } = spec.kind {
                    assert_eq!(
                        stored_number(&display_value(spec, spec.default, &catalog)),
                        spec.default
                    );
                }
            }
        }
        assert_eq!(display_number("4096", Locale::Ar), "٤٠٩٦");
    }
}
