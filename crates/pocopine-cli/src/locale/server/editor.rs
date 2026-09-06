use std::{collections::HashMap, path::Path};

use pocopine_locale::{
    ArgumentKind, Locale, Message,
    server::{SourceMessages, parse_messages},
};
use tower_lsp::lsp_types::*;

use crate::lsp::LineIndex;

#[derive(Clone, Copy)]
enum Form {
    Path,
    Call,
}

struct KeyContext<'a> {
    start: usize,
    key_start: usize,
    key: &'a str,
    form: Form,
}

fn key_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.')
}

fn key_start(source: &str, start: usize) -> Option<(usize, Form)> {
    let tail = source.get(start + 2..)?;
    if tail.starts_with('.') {
        return Some((start + 3, Form::Path));
    }
    if tail.is_empty() {
        return Some((start + 2, Form::Path));
    }
    let tail = tail.trim_start().strip_prefix('(')?.trim_start();
    if !tail.starts_with(['\'', '"']) {
        return None;
    }
    Some((source.len() - tail.len() + 1, Form::Call))
}

fn completion_context(before: &str) -> Option<KeyContext<'_>> {
    let start = before.rfind("$t")?;
    let (key_start, form) = key_start(before, start)?;
    let key = &before[key_start..];
    key.bytes().all(key_char).then_some(KeyContext {
        start,
        key_start,
        key,
        form,
    })
}

fn catalog(project: &Path, open: &HashMap<Url, String>) -> Option<(Locale, SourceMessages)> {
    let config = super::config(project).ok()??;
    let path = project
        .join("locales")
        .join(format!("{}.json", config.default));
    let uri = Url::from_file_path(&path).ok()?;
    let source = open
        .get(&uri)
        .cloned()
        .or_else(|| super::commands::read_text(&path).ok())?;
    Some((config.default, parse_messages(&source, 0).ok()?))
}

fn signature(message: &Message) -> String {
    message
        .arguments()
        .iter()
        .map(|(name, kind)| {
            format!(
                "{name}: {}",
                match kind {
                    ArgumentKind::Text => "text",
                    ArgumentKind::Number => "number",
                    ArgumentKind::DateTime => "date/time",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn documentation(locale: &Locale, key: &str, text: &str, message: &Message) -> MarkupContent {
    let rich = if message.elements().is_empty() {
        String::new()
    } else {
        format!(
            "\nRich text: {} template child elements",
            message.elements().len()
        )
    };
    MarkupContent {
        kind: MarkupKind::PlainText,
        value: format!("{key}({}){rich}\n\n{locale}: {text}", signature(message)),
    }
}

pub fn editor_completions(
    project: &Path,
    text: &str,
    cursor: usize,
    open: &HashMap<Url, String>,
) -> Option<Vec<CompletionItem>> {
    let context = completion_context(text.get(..cursor)?)?;
    let (locale, catalog) = catalog(project, open)?;
    let index = LineIndex::new(text);
    let end = cursor + text[cursor..].bytes().take_while(|b| key_char(*b)).count();
    let mut items = Vec::new();
    for (key, source) in catalog.range(context.key.to_owned()..) {
        if !key.starts_with(context.key) {
            break;
        }
        let Ok(message) = Message::parse(&source.text) else {
            continue;
        };
        let (start, replacement, snippet) = match context.form {
            Form::Call => (context.key_start, key.clone(), false),
            Form::Path if message.arguments().is_empty() => {
                (context.start, format!("$t.{key}"), false)
            }
            Form::Path => {
                let args = message
                    .arguments()
                    .keys()
                    .enumerate()
                    .map(|(i, name)| format!("${{{}:{name}}}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let quote = call_quote(&text[..context.start]);
                (
                    context.start,
                    format!("$t({quote}{key}{quote}, {args})"),
                    true,
                )
            }
        };
        items.push(CompletionItem {
            label: key.clone(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(format!("({})", signature(&message))),
            documentation: Some(Documentation::MarkupContent(documentation(
                &locale,
                key,
                &source.text,
                &message,
            ))),
            filter_text: Some(match context.form {
                Form::Path => format!("$t.{key}"),
                Form::Call => key.clone(),
            }),
            insert_text_format: Some(if snippet {
                InsertTextFormat::SNIPPET
            } else {
                InsertTextFormat::PLAIN_TEXT
            }),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: index.range(&(start..end)),
                new_text: replacement,
            })),
            ..Default::default()
        });
    }
    Some(items)
}

fn call_quote(before: &str) -> char {
    let tag = before.rsplit('<').next().unwrap_or(before);
    let mut quote = None;
    for ch in tag.chars() {
        if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        }
    }
    if quote == Some('\'') { '"' } else { '\'' }
}

pub fn editor_hover(
    project: &Path,
    text: &str,
    cursor: usize,
    open: &HashMap<Url, String>,
) -> Option<Hover> {
    for (start, _) in text.match_indices("$t") {
        let Some((key_start, _)) = key_start(text, start) else {
            continue;
        };
        let end = key_start
            + text[key_start..]
                .bytes()
                .take_while(|b| key_char(*b))
                .count();
        if cursor < start || cursor > end {
            continue;
        }
        let key = &text[key_start..end];
        let (locale, catalog) = catalog(project, open)?;
        let source = catalog.get(key)?;
        let message = Message::parse(&source.text).ok()?;
        return Some(Hover {
            contents: HoverContents::Markup(documentation(&locale, key, &source.text, &message)),
            range: Some(LineIndex::new(text).range(&(start..end))),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("pocopine.toml"),
            "[locale]\ndefault='en'\nlocales=['en']\n",
        )
        .unwrap();
        std::fs::create_dir(temp.path().join("locales")).unwrap();
        std::fs::write(
            temp.path().join("locales/en.json"),
            r#"{"common.welcome":"Hello {name}","common.title":"Title"}"#,
        )
        .unwrap();
        temp
    }

    #[test]
    fn path_completion_inserts_typed_arguments_and_replaces_the_whole_key() {
        let temp = project();
        let text = "😀 <p pp-text=\"$t.common.welcome\">";
        let cursor = text.find("welcome").unwrap() + 3;
        let items = editor_completions(temp.path(), text, cursor, &HashMap::new()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text_format, Some(InsertTextFormat::SNIPPET));
        let CompletionTextEdit::Edit(edit) = items[0].text_edit.as_ref().unwrap() else {
            panic!()
        };
        assert_eq!(edit.new_text, "$t('common.welcome', ${1:name})");
        assert_eq!(
            edit.range.start.character as usize,
            text[..text.find("$t").unwrap()].encode_utf16().count()
        );
        assert_eq!(
            edit.range.end.character as usize,
            text[..text.find("welcome").unwrap() + 7]
                .encode_utf16()
                .count()
        );
        let single = "<p pp-text='$t.common.wel'";
        let item = editor_completions(temp.path(), single, single.len() - 1, &HashMap::new())
            .unwrap()
            .remove(0);
        let Some(CompletionTextEdit::Edit(edit)) = item.text_edit else {
            panic!()
        };
        assert_eq!(edit.new_text, "$t(\"common.welcome\", ${1:name})");
    }

    #[test]
    fn call_completion_and_hover_share_unsaved_default_catalog_text() {
        let temp = project();
        let uri = Url::from_file_path(temp.path().join("locales/en.json")).unwrap();
        let open = [(uri, r#"{"common.welcome":"Welcome back, {name}"}"#.into())].into();
        let text = "<p pp-text=\"$t('common.wel', name)\">";
        let cursor = text.find("wel'").unwrap() + 3;
        let items = editor_completions(temp.path(), text, cursor, &open).unwrap();
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!()
        };
        assert_eq!(edit.new_text, "common.welcome");
        let full = text.replace("common.wel", "common.welcome");
        let hover = editor_hover(temp.path(), &full, cursor, &open).unwrap();
        let HoverContents::Markup(content) = hover.contents else {
            panic!()
        };
        assert_eq!(content.kind, MarkupKind::PlainText);
        assert!(content.value.contains("Welcome back, {name}"));
        assert!(content.value.contains("name: text"));
        assert!(editor_completions(temp.path(), "$t(key", 6, &open).is_none());
    }
}
