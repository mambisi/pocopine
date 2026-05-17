//! Input rules — typing-time pattern matchers that rewrite text into
//! richer structure.
//!
//! An [`InputRule`] pairs a regex with a handler. The view's
//! `beforeinput`/`insertText` handler consults registered rules
//! before defaulting to a plain text insert: if any rule matches
//! the text immediately preceding the cursor (plus the about-to-be-
//! inserted character), its handler is invoked and its returned
//! [`crate::state::Transaction`] is dispatched in place of the
//! default insert.
//!
//! Two flavors of rule cover ~all standard typing-time UX:
//!
//! * **Typography rules** — `--` → `—`, `...` → `…`, smart quotes,
//!   ellipsis. Pure character substitution; the predefined set lives
//!   in [`rules`].
//! * **Block / wrapping rules** — `# ` → heading, `* ` → bullet list,
//!   `> ` → blockquote, ``` ``` ``` → code block. Built via
//!   [`rule_builders::textblock_type_input_rule`] and
//!   [`rule_builders::wrapping_input_rule`].
//!
//! The matching engine ([`run::run_rules`]) is pure: it reads the
//! current [`crate::state::EditorState`] and returns
//! `Option<Transaction>` without dispatching. The view-side
//! integration that drives `run_rules` from `beforeinput` lands in
//! Phase 5 C2; this commit (C1) ships only the data types, the
//! matching function, and the [`plugin::input_rules`] factory.
//!
//! Mirrors `prosemirror-inputrules/src/inputrules.ts` 1:1 in module
//! layout; see that file for the upstream design.

pub mod plugin;
pub mod rule;
pub mod rule_builders;
pub mod rules;
pub mod run;

#[cfg(test)]
mod tests;

pub use plugin::{input_rules, INPUT_RULES_PLUGIN_KEY};
pub use rule::{InCodePolicy, InputRule, RuleHandler, RuleOptions};
pub use rule_builders::{textblock_type_input_rule, wrapping_input_rule};
pub use rules::{
    close_double_quote, close_single_quote, ellipsis, em_dash, open_double_quote,
    open_single_quote, smart_quotes,
};
pub use run::run_rules;
