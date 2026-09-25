/// A desktop application considered for an Open With submenu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenWithApplication {
    label: Box<str>,
    desktop_id: Box<str>,
    compatible: bool,
}

impl OpenWithApplication {
    #[must_use]
    pub fn compatible(label: impl Into<Box<str>>, desktop_id: impl Into<Box<str>>) -> Self {
        Self {
            label: label.into(),
            desktop_id: desktop_id.into(),
            compatible: true,
        }
    }

    #[must_use]
    pub fn incompatible(label: impl Into<Box<str>>, desktop_id: impl Into<Box<str>>) -> Self {
        Self {
            label: label.into(),
            desktop_id: desktop_id.into(),
            compatible: false,
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn desktop_id(&self) -> &str {
        &self.desktop_id
    }

    #[must_use]
    pub const fn is_compatible(&self) -> bool {
        self.compatible
    }
}
