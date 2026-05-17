//! Smart-typography extension — bundles em-dash, ellipsis, and the
//! four smart-quote variants as input rules.
//!
//! Apps that want typing-time character substitutions
//! (`--` → `—`, `...` → `…`, smart quotes) register this extension
//! either via [`crate::extension::register`] (default-runtime path)
//! or via [`crate::runtime::RuntimeBuilder::with`] (per-runtime).
//!
//! Contributes only input rules — no nodes, marks, commands, key
//! bindings, plugins, or node-views.

use crate::extension::RichTextExtension;
use crate::inputrules::{self, InputRule};

pub struct SmartTypographyExtension;

impl RichTextExtension for SmartTypographyExtension {
    fn name(&self) -> &str {
        "smart-typography"
    }

    fn input_rules(&self) -> Vec<InputRule> {
        let mut rules = vec![inputrules::em_dash(), inputrules::ellipsis()];
        rules.extend(inputrules::smart_quotes());
        rules
    }
}
