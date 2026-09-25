#[must_use]
pub fn status_text_with_size(
    item_count: usize,
    selection_count: usize,
    selected_bytes: Option<u64>,
    complete: bool,
) -> String {
    if selection_count > 0 {
        let mut text = if selection_count == 1 {
            "1 item selected".to_owned()
        } else {
            format!("{selection_count} items selected")
        };
        if let Some(bytes) = selected_bytes {
            text.push_str(" — ");
            text.push_str(&format_bytes(bytes));
        }
        return text;
    }
    if !complete {
        return format!("{item_count} items loaded — total unknown");
    }
    match (item_count, selection_count) {
        (1, 0) => "1 item".into(),
        (count, 0) => format!("{count} items"),
        (_, 1) => "1 item selected".into(),
        (_, selected) => format!("{selected} items selected"),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_distinguishes_partial_totals_and_known_selected_size() {
        assert_eq!(
            status_text_with_size(512, 0, None, false),
            "512 items loaded — total unknown"
        );
        assert_eq!(
            status_text_with_size(512, 2, Some(2_048), false),
            "2 items selected — 2.0 KiB"
        );
    }
}
