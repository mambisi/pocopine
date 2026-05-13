use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeepLabelOption {
    pub name: String,
    pub selected: bool,
    pub visible: bool,
}

pub(crate) fn normalize_label(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    Some(label.to_string())
}

pub(crate) fn normalize_labels(labels: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for label in labels {
        let Some(label) = normalize_label(&label) else {
            continue;
        };
        if !normalized.iter().any(|existing| existing == &label) {
            normalized.push(label);
        }
    }
    normalized
}

pub fn label_options_for(labels: &[String], selected: &[String]) -> Vec<KeepLabelOption> {
    label_picker_options_for(labels, selected, "").0
}

pub fn label_picker_options_for(
    labels: &[String],
    selected: &[String],
    query: &str,
) -> (Vec<KeepLabelOption>, bool) {
    let labels = normalize_labels(labels.to_vec());
    let query = query.trim();
    let needle = query.to_lowercase();
    let can_create = normalize_label(query).is_some()
        && !labels.iter().any(|label| label.eq_ignore_ascii_case(query));

    let options = labels
        .into_iter()
        .map(|name| {
            let visible = needle.is_empty() || name.to_lowercase().contains(&needle);
            KeepLabelOption {
                selected: selected.iter().any(|label| label == &name),
                visible,
                name,
            }
        })
        .collect();

    (options, can_create)
}

pub fn can_create_label(labels: &[String], query: &str) -> bool {
    normalize_labels(labels.to_vec())
        .iter()
        .all(|label| !label.eq_ignore_ascii_case(query.trim()))
        && normalize_label(query).is_some()
}
