use musheen_core::{DisplayPath, StorePath};
use std::path::{Component, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Breadcrumb {
    label: DisplayPath,
    target: StorePath,
}

impl Breadcrumb {
    #[must_use]
    pub fn label(&self) -> &DisplayPath {
        &self.label
    }

    #[must_use]
    pub fn target(&self) -> &StorePath {
        &self.target
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BreadcrumbTrail {
    hidden: Vec<Breadcrumb>,
    visible: Vec<Breadcrumb>,
}

impl BreadcrumbTrail {
    #[must_use]
    pub fn from_path(path: &StorePath, max_visible: usize) -> Self {
        let crumbs = unix_crumbs(path).unwrap_or_else(|| {
            vec![Breadcrumb {
                label: DisplayPath::from_store_path(path),
                target: path.clone(),
            }]
        });
        let visible_count = max_visible.max(1).min(crumbs.len());
        let split = crumbs.len() - visible_count;
        let mut crumbs = crumbs;
        let visible = crumbs.split_off(split);
        Self {
            hidden: crumbs,
            visible,
        }
    }

    #[must_use]
    pub fn hidden(&self) -> &[Breadcrumb] {
        &self.hidden
    }

    #[must_use]
    pub fn visible(&self) -> &[Breadcrumb] {
        &self.visible
    }
}

fn unix_crumbs(path: &StorePath) -> Option<Vec<Breadcrumb>> {
    let path = path.as_unix_path()?;
    let mut target = PathBuf::new();
    let mut crumbs = Vec::new();
    for component in path.components() {
        target.push(component.as_os_str());
        let label = match component {
            Component::RootDir => DisplayPath::new("/"),
            _ => DisplayPath::from(component.as_os_str()),
        };
        crumbs.push(Breadcrumb {
            label,
            target: StorePath::from_unix_path(target.clone().into_os_string()),
        });
    }
    Some(crumbs)
}
