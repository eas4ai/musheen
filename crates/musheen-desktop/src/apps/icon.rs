use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// App-owned desktop-entry icon reference. Provider crate types never escape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationIcon {
    Name(Box<str>),
    Path(PathBuf),
}

impl ApplicationIcon {
    #[must_use]
    pub fn name(value: impl Into<Box<str>>) -> Self {
        Self::Name(value.into())
    }

    #[must_use]
    pub fn path(value: impl Into<PathBuf>) -> Self {
        Self::Path(value.into())
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        let path = Path::new(value);
        if path.is_absolute() {
            return Some(Self::path(path));
        }
        (!value.is_empty()
            && value.len() <= 255
            && !value.starts_with('.')
            && !value.contains(['/', '\\'])
            && !value.chars().any(char::is_control))
        .then(|| Self::name(value))
    }

    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        match self {
            Self::Name(name) => OsStr::new(name.as_ref()),
            Self::Path(path) => path.as_os_str(),
        }
    }
}

/// Replaceable boundary for the selected freedesktop icon implementation.
pub trait ApplicationIconProvider: Send + Sync {
    fn resolve(&self, icon: &ApplicationIcon) -> Option<PathBuf>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FreedesktopIconProvider;

impl ApplicationIconProvider for FreedesktopIconProvider {
    fn resolve(&self, icon: &ApplicationIcon) -> Option<PathBuf> {
        let path = match icon {
            ApplicationIcon::Name(name) => freedesktop::get_icon(name)?,
            ApplicationIcon::Path(path) => path.clone(),
        };
        path.is_absolute()
            .then_some(path)
            .filter(|path| path.is_file())
    }
}
