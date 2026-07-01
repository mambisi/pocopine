use anyhow::Result;

use super::FrameworkEvent;

pub fn render_jsonl(events: &[FrameworkEvent]) -> Result<Vec<String>> {
    events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FrameworkEvent;

    #[test]
    fn event_serializes_as_json_line() {
        let lines = render_jsonl(&[FrameworkEvent::assistant_text("hello")]).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(r#""event":"assistant_text""#));
    }
}
