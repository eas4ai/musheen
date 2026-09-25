use crate::EntrySnapshot;

#[must_use]
pub fn source_unchanged(before: &EntrySnapshot, after: &EntrySnapshot) -> bool {
    before.identity() == after.identity()
        && before.kind() == after.kind()
        && before.size() == after.size()
        && before.filesystem_id() == after.filesystem_id()
}
