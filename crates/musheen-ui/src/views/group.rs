use musheen_core::{ItemKind, StoreItem};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum GroupKey {
    #[default]
    None,
    Kind,
    FirstLetter,
    Modified,
}

pub(crate) fn compare(key: GroupKey, left: &StoreItem, right: &StoreItem) -> Ordering {
    match key {
        GroupKey::None => Ordering::Equal,
        GroupKey::Kind => kind_rank(left.kind()).cmp(&kind_rank(right.kind())),
        GroupKey::FirstLetter => first_letter(left).cmp(&first_letter(right)),
        GroupKey::Modified => left
            .modified_unix_seconds()
            .unwrap_or(i64::MIN)
            .cmp(&right.modified_unix_seconds().unwrap_or(i64::MIN)),
    }
}

fn kind_rank(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Directory => 0,
        ItemKind::RegularFile => 1,
        ItemKind::SymbolicLink => 2,
        ItemKind::Other => 3,
    }
}

fn first_letter(item: &StoreItem) -> Option<char> {
    item.display_name()
        .as_str()
        .chars()
        .next()
        .map(|value| value.to_ascii_uppercase())
}
