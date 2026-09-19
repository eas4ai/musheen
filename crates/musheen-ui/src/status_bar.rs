#[must_use]
pub fn status_text(item_count: usize, selection_count: usize) -> String {
    match (item_count, selection_count) {
        (1, 0) => "1 item".into(),
        (count, 0) => format!("{count} items"),
        (_, 1) => "1 item selected".into(),
        (_, selected) => format!("{selected} items selected"),
    }
}
