#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationChoice {
    desktop_id: Box<str>,
    name: Box<str>,
    compatible: bool,
}

impl ApplicationChoice {
    pub fn new(
        desktop_id: impl Into<Box<str>>,
        name: impl Into<Box<str>>,
        compatible: bool,
    ) -> Self {
        Self {
            desktop_id: desktop_id.into(),
            name: name.into(),
            compatible,
        }
    }

    pub fn desktop_id(&self) -> &str {
        &self.desktop_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn compatible(&self) -> bool {
        self.compatible
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenWithIntent {
    OpenOnce,
    SetAsDefault,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenWithPlan {
    desktop_id: Box<str>,
    set_as_default: bool,
}

impl OpenWithPlan {
    pub fn desktop_id(&self) -> &str {
        &self.desktop_id
    }

    pub fn set_as_default(&self) -> bool {
        self.set_as_default
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenWithModel {
    mime_type: Box<str>,
    applications: Vec<ApplicationChoice>,
    selected: Option<Box<str>>,
}

impl OpenWithModel {
    pub fn new(mime_type: impl Into<Box<str>>, applications: Vec<ApplicationChoice>) -> Self {
        Self {
            mime_type: mime_type.into(),
            applications,
            selected: None,
        }
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn compatible_applications(&self) -> Vec<&ApplicationChoice> {
        self.applications
            .iter()
            .filter(|application| application.compatible)
            .collect()
    }

    pub fn select(&mut self, desktop_id: &str) -> Result<(), OpenWithError> {
        let application = self
            .applications
            .iter()
            .find(|application| application.desktop_id() == desktop_id)
            .ok_or(OpenWithError::UnknownApplication)?;
        if !application.compatible() {
            return Err(OpenWithError::IncompatibleApplication);
        }
        self.selected = Some(desktop_id.into());
        Ok(())
    }

    pub fn plan(&self, intent: OpenWithIntent) -> Result<OpenWithPlan, OpenWithError> {
        let desktop_id = self.selected.clone().ok_or(OpenWithError::NoSelection)?;
        Ok(OpenWithPlan {
            desktop_id,
            set_as_default: intent == OpenWithIntent::SetAsDefault,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenWithError {
    UnknownApplication,
    IncompatibleApplication,
    NoSelection,
}
