use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

const EN_US: &str = include_str!("../../../locales/en-US.ftl");
const EN_XA: &str = include_str!("../../../locales/en-XA.ftl");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Locale {
    #[default]
    EnUs,
    EnXa,
}

impl Locale {
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::EnXa => "en-XA",
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
        };
        Ok(Self {
            locale,
            messages: parse_catalog(source)?,
        })
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
}
