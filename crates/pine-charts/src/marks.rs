pub(crate) fn color_or_current(value: &str) -> String {
    color_or(value, "currentColor")
}

pub(crate) fn color_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.into()
    } else {
        trimmed.into()
    }
}

pub(crate) fn text_anchor(value: &str) -> &'static str {
    match value.trim() {
        "start" => "start",
        "end" => "end",
        _ => "middle",
    }
}

pub(crate) fn rotation_transform(angle: f64, x: f64, y: f64) -> String {
    if angle.abs() <= f64::EPSILON {
        String::new()
    } else {
        format!("rotate({angle} {x} {y})")
    }
}

pub(crate) fn label_or_key(label: &str, key: &str, index: usize) -> String {
    if label.trim().is_empty() {
        key_or_index("item", key, index)
    } else {
        label.trim().into()
    }
}

pub(crate) fn key_or_index(prefix: &str, key: &str, index: usize) -> String {
    if key.trim().is_empty() {
        format!("{prefix}-{index}")
    } else {
        key.trim().into()
    }
}
