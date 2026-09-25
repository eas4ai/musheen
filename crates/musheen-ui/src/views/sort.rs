use musheen_core::{ItemKind, StoreItem};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Kind,
    Modified,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SortSpec {
    pub key: SortKey,
    pub direction: SortDirection,
}

pub(crate) fn compare(spec: SortSpec, left: &StoreItem, right: &StoreItem) -> Ordering {
    let ordering = match spec.key {
        SortKey::Name => apply_direction(
            natural_compare(left.display_name().as_str(), right.display_name().as_str()),
            spec.direction,
        ),
        SortKey::Size => compare_known(left.size(), right.size(), spec.direction),
        SortKey::Kind => apply_direction(
            kind_rank(left.kind()).cmp(&kind_rank(right.kind())),
            spec.direction,
        ),
        SortKey::Modified => compare_known(
            left.modified_unix_seconds(),
            right.modified_unix_seconds(),
            spec.direction,
        ),
    };
    ordering
        .then_with(|| natural_compare(left.display_name().as_str(), right.display_name().as_str()))
}

fn compare_known<T: Ord>(left: Option<T>, right: Option<T>, direction: SortDirection) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => apply_direction(left.cmp(&right), direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn apply_direction(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => ordering,
        SortDirection::Descending => ordering.reverse(),
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

pub(crate) fn natural_compare(left: &str, right: &str) -> Ordering {
    let mut left = left.char_indices().peekable();
    let mut right = right.char_indices().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some((_, left_char)), Some((_, right_char)))
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() =>
            {
                let left_number = take_number(&mut left);
                let right_number = take_number(&mut right);
                let left_significant = left_number.trim_start_matches('0');
                let right_significant = right_number.trim_start_matches('0');
                let ordering = left_significant
                    .len()
                    .cmp(&right_significant.len())
                    .then_with(|| left_significant.cmp(right_significant))
                    .then_with(|| left_number.len().cmp(&right_number.len()));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some((_, left_char)), Some((_, right_char))) => {
                left.next();
                right.next();
                let ordering = left_char
                    .to_ascii_lowercase()
                    .cmp(&right_char.to_ascii_lowercase());
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

fn take_number(iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> String {
    let mut number = String::new();
    while let Some((_, value)) = iter.peek().copied() {
        if !value.is_ascii_digit() {
            break;
        }
        iter.next();
        number.push(value);
    }
    number
}
