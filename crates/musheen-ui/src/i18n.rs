use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

const EN_US: &str = include_str!("../../../locales/en-US.ftl");
const EN_XA: &str = include_str!("../../../locales/en-XA.ftl");
const AR: &str = include_str!("../../../locales/ar.ftl");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Locale {
    #[default]
    EnUs,
    EnXa,
    /// Right-to-left system locale. English catalog fallback keeps every
    /// command available while translations are supplied by the system.
    Ar,
}

impl Locale {
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::EnXa => "en-XA",
            Self::Ar => "ar",
        }
    }

    #[must_use]
    pub fn from_environment() -> Self {
        ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find_map(|value| Self::from_preferences(&value))
            .unwrap_or(Self::EnUs)
    }

    fn from_preferences(value: &str) -> Option<Self> {
        value.split(':').find_map(|candidate| {
            let tag = candidate.split(['.', '@']).next()?;
            if tag.eq_ignore_ascii_case("en-XA") || tag.eq_ignore_ascii_case("en_XA") {
                Some(Self::EnXa)
            } else if tag.eq_ignore_ascii_case("ar") || tag.starts_with("ar_") {
                Some(Self::Ar)
            } else if tag.eq_ignore_ascii_case("en-US") || tag.eq_ignore_ascii_case("en_US") {
                Some(Self::EnUs)
            } else {
                None
            }
        })
    }
}

#[derive(Clone, Debug)]
pub struct Catalog {
    locale: Locale,
    messages: BTreeMap<Box<str>, Box<str>>,
}

impl Catalog {
    pub fn load(locale: Locale) -> Result<Self, CatalogError> {
        let source = match locale {
            Locale::EnUs => EN_US,
            Locale::EnXa => EN_XA,
            Locale::Ar => EN_US,
        };
        let mut messages = parse_catalog(source)?;
        if locale == Locale::Ar {
            messages.extend(parse_catalog(AR)?);
        }
        Ok(Self { locale, messages })
    }

    pub fn system() -> Result<Self, CatalogError> {
        Self::load(Locale::from_environment())
    }

    #[must_use]
    pub const fn locale(&self) -> Locale {
        self.locale
    }

    pub fn message(&self, id: &str) -> Result<&str, CatalogError> {
        let normalized = id.replace('.', "-");
        self.messages
            .get(normalized.as_str())
            .map(AsRef::as_ref)
            .ok_or_else(|| CatalogError::MissingMessage(id.into()))
    }

    #[must_use]
    pub fn message_ids(&self) -> Vec<&str> {
        self.messages.keys().map(AsRef::as_ref).collect()
    }

    /// Translate owned domain refusals without changing provider error detail.
    #[must_use]
    pub fn localize_reason(&self, reason: &str) -> String {
        static ENGLISH: std::sync::LazyLock<BTreeMap<Box<str>, Box<str>>> =
            std::sync::LazyLock::new(|| parse_catalog(EN_US).expect("English catalog is valid"));
        ENGLISH
            .iter()
            .find(|(_, value)| value.as_ref() == reason)
            .and_then(|(id, _)| self.message(id).ok())
            .unwrap_or(reason)
            .to_owned()
    }

    #[must_use]
    pub fn unavailable_label(&self, label: &str, reason: &str) -> String {
        let unavailable = self
            .message("catalog-unavailable")
            .expect("the unavailable catalog message exists");
        let reason = self.localize_reason(reason);
        if self.locale == Locale::Ar {
            format!("{label}، {unavailable}: {reason}")
        } else {
            format!("{label}, {unavailable}: {reason}")
        }
    }
}

fn parse_catalog(source: &str) -> Result<BTreeMap<Box<str>, Box<str>>, CatalogError> {
    let mut messages = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((id, value)) = line.split_once('=') else {
            return Err(CatalogError::InvalidLine(index + 1));
        };
        let id = id.trim();
        let value = value.trim();
        if id.is_empty()
            || value.is_empty()
            || !id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(CatalogError::InvalidLine(index + 1));
        }
        if messages.insert(id.into(), value.into()).is_some() {
            return Err(CatalogError::DuplicateMessage(id.into()));
        }
    }
    Ok(messages)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    InvalidLine(usize),
    DuplicateMessage(Box<str>),
    MissingMessage(Box<str>),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLine(line) => write!(formatter, "invalid locale message on line {line}"),
            Self::DuplicateMessage(id) => write!(formatter, "duplicate locale message {id}"),
            Self::MissingMessage(id) => write!(formatter, "missing locale message {id}"),
        }
    }
}

impl Error for CatalogError {}

#[cfg(test)]
mod tests {
    use super::{Catalog, Locale};

    #[test]
    fn locale_preferences_accept_posix_and_language_list_forms() {
        assert_eq!(
            Locale::from_preferences("fr_FR:en_XA.UTF-8"),
            Some(Locale::EnXa)
        );
        assert_eq!(
            Locale::from_preferences("en_US.UTF-8@calendar=gregorian"),
            Some(Locale::EnUs)
        );
        assert_eq!(Locale::from_preferences("fr_FR.UTF-8"), None);
    }

    #[test]
    fn pseudo_locale_covers_context_menu_dialog_chrome() {
        let catalog = Catalog::load(Locale::EnXa).expect("pseudo catalog loads");
        for key in [
            "menu-more",
            "dialog-choose-destination",
            "dialog-destination-explanation",
            "dialog-cancel",
            "dialog-review-operation",
            "dialog-continue",
            "dialog-authorization-unavailable",
            "dialog-command",
            "dialog-targets",
            "dialog-move-review",
            "dialog-operation-review",
            "open-with-title",
            "open-with-open-once",
            "open-with-set-default-and-open",
            "application-open-no-targets",
            "application-open-detection-failed",
            "application-open-provider-unsupported",
            "application-open-mixed-mime",
            "application-open-association-unavailable",
            "application-open-no-default",
            "application-open-application-unavailable",
            "application-open-launch-failed",
            "application-open-dialog-unavailable",
            "application-open-loading",
            "application-open-terminal-unavailable",
            "application-open-unrepresentable-target",
            "application-open-try-exec-unavailable",
            "application-open-persistence-failed",
            "application-open-spawn-failed",
            "application-open-recovery",
            "application-open-items",
            "context-backend-unavailable",
            "context-target-changed",
        ] {
            assert!(
                catalog
                    .message(key)
                    .expect("context menu dialog key exists")
                    .starts_with('⟦'),
                "{key} is pseudo-localized"
            );
        }
    }

    #[test]
    fn catalog_and_provider_properties_messages_cover_every_shipped_locale() {
        let keys = [
            "catalog-home",
            "catalog-recent-locations",
            "catalog-pinned",
            "catalog-storage",
            "catalog-tags",
            "catalog-unpin",
            "catalog-unavailable",
            "catalog-orphan-review",
            "catalog-orphan-heading",
            "catalog-remove-metadata",
            "catalog-remove-reviewed-orphan",
            "catalog-tag-name",
            "catalog-rename-tag",
            "catalog-rename",
            "catalog-tag-editor",
            "catalog-apply-tags",
            "provider-properties-general",
            "provider-properties-tags",
            "provider-properties-remove",
            "provider-properties-some-items",
            "provider-properties-add-tag",
            "provider-properties-apply",
            "provider-properties-provider",
            "provider-properties-stable-identity",
            "provider-properties-location",
            "provider-properties-capability",
            "provider-properties-tag-storage-unavailable",
            "provider-properties-tags-unavailable",
            "provider-properties-page-unavailable",
            "provider-properties-supported",
            "provider-properties-unsupported",
            "provider-properties-unknown",
            "provider-properties-mixed",
            "provider-properties-unavailable",
        ];
        let english = Catalog::load(Locale::EnUs).expect("English catalog loads");
        let arabic = Catalog::load(Locale::Ar).expect("Arabic catalog loads");
        let pseudo = Catalog::load(Locale::EnXa).expect("pseudo catalog loads");

        for key in keys {
            let english_message = english.message(key).expect("English message exists");
            let arabic_message = arabic.message(key).expect("Arabic message exists");
            let pseudo_message = pseudo.message(key).expect("pseudo message exists");
            assert_ne!(
                arabic_message, english_message,
                "{key} is translated to Arabic"
            );
            assert!(pseudo_message.starts_with('⟦'), "{key} is pseudo-localized");
            assert!(pseudo_message.ends_with('⟧'), "{key} is pseudo-localized");
        }
        for key in english
            .message_ids()
            .into_iter()
            .filter(|key| key.starts_with("properties-") || key.starts_with("catalog-error-move-"))
        {
            let english_message = english.message(key).expect("English message exists");
            let arabic_message = arabic.message(key).expect("Arabic message exists");
            let pseudo_message = pseudo.message(key).expect("pseudo message exists");
            assert_ne!(
                arabic_message, english_message,
                "{key} is translated to Arabic"
            );
            assert!(pseudo_message.starts_with('⟦'), "{key} is pseudo-localized");
            assert!(pseudo_message.ends_with('⟧'), "{key} is pseudo-localized");
        }
    }
}
