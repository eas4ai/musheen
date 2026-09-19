use musheen_core::StorePath;
use std::ffi::OsString;
use std::path::Path;

#[must_use]
pub fn resolve_path_input(current: &StorePath, input: &str) -> Option<StorePath> {
    let current = current.as_unix_path()?;
    let input = Path::new(input);
    if input.as_os_str().is_empty() {
        return None;
    }
    let resolved = if input.is_absolute() {
        input.to_path_buf()
    } else {
        current.join(input)
    };
    Some(StorePath::from_unix_path(resolved.into_os_string()))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OmnibarMode {
    #[default]
    Path,
    Search,
    Command,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OmnibarSubmission {
    Path(String),
    Search(String),
    Command(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OmnibarState {
    mode: OmnibarMode,
    text: String,
}

impl OmnibarState {
    pub fn enter(&mut self, mode: OmnibarMode, text: impl Into<String>) {
        self.mode = mode;
        self.text = text.into();
    }

    #[must_use]
    pub const fn mode(&self) -> OmnibarMode {
        self.mode
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn submit(&self) -> OmnibarSubmission {
        match self.mode {
            OmnibarMode::Path => OmnibarSubmission::Path(self.text.clone()),
            OmnibarMode::Search => OmnibarSubmission::Search(self.text.clone()),
            OmnibarMode::Command => OmnibarSubmission::Command(self.text.clone()),
        }
    }

    pub fn cancel(&mut self) {
        self.mode = OmnibarMode::Path;
        self.text.clear();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathSuggestion {
    label: String,
    target: StorePath,
}

impl PathSuggestion {
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn target(&self) -> &StorePath {
        &self.target
    }
}

#[must_use]
pub fn suggest_local_paths(
    current: &StorePath,
    input: &str,
    children: &[OsString],
) -> Vec<PathSuggestion> {
    let Some(current) = current.as_unix_path() else {
        return Vec::new();
    };
    let input_path = Path::new(input);
    let prefix = input_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let relative_parent = input_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());

    children
        .iter()
        .filter(|name| name.to_string_lossy().starts_with(prefix))
        .map(|name| {
            let mut target = current.to_path_buf();
            if let Some(parent) = relative_parent {
                target.push(parent);
            }
            target.push(name);
            PathSuggestion {
                label: name.to_string_lossy().into_owned(),
                target: StorePath::from_unix_path(target.into_os_string()),
            }
        })
        .collect()
}
