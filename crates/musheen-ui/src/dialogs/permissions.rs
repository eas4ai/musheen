use musheen_desktop::{AggregateValue, PropertySnapshot};
use musheen_ops::{MetadataChange, MetadataScope};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionsPageModel {
    owner: AggregateValue<u32>,
    group: AggregateValue<u32>,
    mode: AggregateValue<u32>,
    change: MetadataChange,
    scope: MetadataScope,
    validation: PermissionValidation,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PermissionValidation {
    owner: Option<Box<str>>,
    group: Option<Box<str>>,
    file_mode: Option<Box<str>>,
    directory_mode: Option<Box<str>>,
}

impl PermissionsPageModel {
    pub(crate) fn from_snapshot(snapshot: &PropertySnapshot) -> Self {
        Self {
            owner: snapshot.aggregate().owner(),
            group: snapshot.aggregate().group(),
            mode: snapshot.aggregate().mode(),
            change: MetadataChange::new(),
            scope: MetadataScope::Single,
            validation: PermissionValidation::default(),
        }
    }

    pub fn owner(&self) -> &AggregateValue<u32> {
        &self.owner
    }

    pub fn group(&self) -> &AggregateValue<u32> {
        &self.group
    }

    pub fn mode(&self) -> &AggregateValue<u32> {
        &self.mode
    }

    pub fn edit_disabled_reason(&self) -> Option<&str> {
        [
            self.validation.owner.as_deref(),
            self.validation.group.as_deref(),
            self.validation.file_mode.as_deref(),
            self.validation.directory_mode.as_deref(),
        ]
        .into_iter()
        .flatten()
        .next()
    }

    pub fn set_file_mode(&mut self, mode: u32) {
        self.change = std::mem::take(&mut self.change).with_file_mode(mode);
        self.validation.file_mode = None;
    }

    pub fn set_directory_mode(&mut self, mode: u32) {
        self.change = std::mem::take(&mut self.change).with_directory_mode(mode);
        self.validation.directory_mode = None;
    }

    pub fn set_owner(&mut self, owner: u32) {
        self.change = std::mem::take(&mut self.change).with_owner(owner);
        self.validation.owner = None;
    }

    pub fn set_group(&mut self, group: u32) {
        self.change = std::mem::take(&mut self.change).with_group(group);
        self.validation.group = None;
    }

    pub fn set_file_mode_text(&mut self, value: &str) {
        match parse_mode(value) {
            Ok(mode) => self.set_file_mode(mode),
            Err(error) => self.validation.file_mode = Some(error.into()),
        }
    }

    pub fn set_directory_mode_text(&mut self, value: &str) {
        match parse_mode(value) {
            Ok(mode) => self.set_directory_mode(mode),
            Err(error) => self.validation.directory_mode = Some(error.into()),
        }
    }

    pub fn set_owner_text(&mut self, value: &str) {
        match parse_id(value, "owner") {
            Ok(owner) => self.set_owner(owner),
            Err(error) => self.validation.owner = Some(error.into()),
        }
    }

    pub fn set_group_text(&mut self, value: &str) {
        match parse_id(value, "group") {
            Ok(group) => self.set_group(group),
            Err(error) => self.validation.group = Some(error.into()),
        }
    }

    pub fn set_single(&mut self) {
        self.scope = MetadataScope::Single;
    }

    pub fn set_recursive(&mut self, include_nested_mounts: bool) {
        self.scope = MetadataScope::recursive(include_nested_mounts, false);
    }

    pub fn review_recursive_scope(&mut self) {
        self.scope = self.scope.reviewed();
    }

    pub fn is_dirty(&self) -> bool {
        self.change.is_dirty()
    }

    pub fn is_valid(&self) -> bool {
        self.edit_disabled_reason().is_none() && self.change.is_valid() && self.scope.is_reviewed()
    }

    pub fn change(&self) -> &MetadataChange {
        &self.change
    }

    pub fn scope(&self) -> MetadataScope {
        self.scope
    }
}

fn parse_mode(value: &str) -> Result<u32, &'static str> {
    let value = value.trim().trim_start_matches("0o");
    let mode = u32::from_str_radix(value, 8).map_err(|_| "mode must be an octal number")?;
    (mode & !0o7777 == 0)
        .then_some(mode)
        .ok_or("mode must be between 0000 and 7777")
}

fn parse_id(value: &str, field: &'static str) -> Result<u32, &'static str> {
    value.trim().parse::<u32>().map_err(|_| match field {
        "owner" => "owner must be a numeric user ID",
        _ => "group must be a numeric group ID",
    })
}
