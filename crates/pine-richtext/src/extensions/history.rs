//! History extension: contributes the history plugin, `undo`/`redo`
//! named commands, and the standard `Mod-z` / `Mod-Shift-z` key
//! bindings.

use std::sync::Arc;

use serde_json::Value;

use crate::commands::BoxedCommand;
use crate::extension::{KeyBindingFactory, KeyBindings, NamedCommand, RichTextExtension};
use crate::history::{history_plugin, redo, undo};
use crate::state::Plugin;

pub struct HistoryExtension;

impl RichTextExtension for HistoryExtension {
    fn name(&self) -> &str {
        "history"
    }

    fn plugins(&self) -> Vec<Plugin> {
        vec![history_plugin()]
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        let undo_factory: NamedCommand =
            Arc::new(|_args: Value| -> Option<BoxedCommand> { Some(undo()) });
        let redo_factory: NamedCommand =
            Arc::new(|_args: Value| -> Option<BoxedCommand> { Some(redo()) });
        vec![("undo".into(), undo_factory), ("redo".into(), redo_factory)]
    }

    fn key_bindings(&self) -> KeyBindings {
        let undo_kb: KeyBindingFactory = Arc::new(undo);
        let redo_kb: KeyBindingFactory = Arc::new(redo);
        vec![("Mod-z".into(), undo_kb), ("Mod-Shift-z".into(), redo_kb)]
    }
}
