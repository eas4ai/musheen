use musheen_desktop::{AggregateValue, PropertySnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionsPageModel {
    owner: AggregateValue<u32>,
    group: AggregateValue<u32>,
    mode: AggregateValue<u32>,
    edit_disabled_reason: Box<str>,
}

impl PermissionsPageModel {
    pub(crate) fn from_snapshot(snapshot: &PropertySnapshot) -> Self {
        Self {
            owner: snapshot.aggregate().owner(),
            group: snapshot.aggregate().group(),
            mode: snapshot.aggregate().mode(),
            edit_disabled_reason:
                "Permission editing is available after safe operations are enabled".into(),
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
        Some(&self.edit_disabled_reason)
    }
}
