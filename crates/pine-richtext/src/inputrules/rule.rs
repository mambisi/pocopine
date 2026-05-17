//! [`InputRule`] data type and the supporting handler / options
//! definitions. Mirrors `prosemirror-inputrules/src/inputrules.ts`'s
//! `InputRule` class.

use std::sync::Arc;

use regex::{Captures, Regex};

use crate::model::Fragment;
use crate::state::{EditorState, Transaction};

/// Handler invoked when a rule's regex matches. Receives the current
/// [`EditorState`], the regex `Captures`, the model `start` position
/// of the match (within the parent textblock content), and the
/// `end` position (the cursor right after the just-typed
/// character). Returns the [`Transaction`] to dispatch, or `None`
/// when the match should be treated as "no rule fired" — the next
/// rule (if any) is tried, otherwise the default text insert wins.
pub type RuleHandler = Arc<
    dyn for<'a> Fn(&'a EditorState, &'a Captures<'a>, usize, usize) -> Option<Transaction>
        + Send
        + Sync
        + 'static,
>;

/// Whether a rule applies inside code-shaped contexts.
///
/// Code blocks (`code_block`) and code marks (`code`) usually want
/// typed characters preserved verbatim — `# ` shouldn't become a
/// heading inside a code block; `--` shouldn't become an em-dash
/// inside an inline `code` mark. Rules opt in/out via this policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InCodePolicy {
    /// Do NOT fire inside code (the default for typography and
    /// markdown shortcuts).
    Disallow,
    /// Fire both inside and outside code contexts.
    Allow,
    /// Fire ONLY inside code contexts. Used by rules that exist
    /// specifically to escape from code (rare).
    Only,
}

/// Compile-time and runtime options on an [`InputRule`].
#[derive(Clone)]
pub struct RuleOptions {
    /// When false, [`crate::commands::undo_input_rule`] (Phase 5 C2)
    /// won't roll the rule back via Backspace.
    pub undoable: bool,
    /// Whether the rule fires inside `code_block` nodes (and other
    /// node-types whose schema spec marks them as `code`).
    pub in_code: InCodePolicy,
    /// Whether the rule fires inside `code` marks (inline code).
    /// The default is `Disallow` — typography rules don't fire
    /// inside `<code>some -- text</code>`.
    pub in_code_mark: InCodePolicy,
}

impl Default for RuleOptions {
    fn default() -> Self {
        Self {
            undoable: true,
            in_code: InCodePolicy::Disallow,
            in_code_mark: InCodePolicy::Disallow,
        }
    }
}

/// A single typing-time rule. Created via [`InputRule::new`] (closure
/// handler) or [`InputRule::replacement`] (literal-string handler).
///
/// Mirrors `prosemirror-inputrules`'s `InputRule` class. The regex's
/// `$` anchor is important — rules match the text in front of the
/// cursor, with the just-typed character appended. Patterns that
/// don't end with `$` will match anywhere in the buffer, which is
/// almost never what you want.
#[derive(Clone)]
pub struct InputRule {
    /// Regex evaluated against the cursor-adjacent text plus the
    /// about-to-be-inserted character.
    pub(crate) regex: Regex,
    pub(crate) handler: RuleHandler,
    pub(crate) options: RuleOptions,
}

impl std::fmt::Debug for InputRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputRule")
            .field("regex", &self.regex.as_str())
            .field("undoable", &self.options.undoable)
            .field("in_code", &self.options.in_code)
            .field("in_code_mark", &self.options.in_code_mark)
            .finish_non_exhaustive()
    }
}

impl InputRule {
    /// Build a rule from a precompiled regex and a closure handler.
    pub fn new<F>(regex: Regex, handler: F) -> Self
    where
        F: for<'a> Fn(&'a EditorState, &'a Captures<'a>, usize, usize) -> Option<Transaction>
            + Send
            + Sync
            + 'static,
    {
        Self {
            regex,
            handler: Arc::new(handler),
            options: RuleOptions::default(),
        }
    }

    /// Build a rule whose effect is to replace the matched text with
    /// a literal `replacement` string. Mirrors upstream's
    /// `stringHandler`. If the regex has a capture group, only the
    /// captured slice is replaced; otherwise the entire match is.
    pub fn replacement(regex: Regex, replacement: impl Into<String>) -> Self {
        let replacement: String = replacement.into();
        Self::new(regex, move |state, captures, start, end| {
            string_handler(&replacement, state, captures, start, end)
        })
    }

    /// Override the default `RuleOptions`. Fluent setter for
    /// builders that want to disable undoability or change code-
    /// context policies.
    pub fn with_options(mut self, options: RuleOptions) -> Self {
        self.options = options;
        self
    }

    /// Disable undo-on-backspace for this rule (matches upstream's
    /// `{undoable: false}`).
    pub fn not_undoable(mut self) -> Self {
        self.options.undoable = false;
        self
    }

    /// Set the `in_code` policy. Defaults to `Disallow`.
    pub fn in_code(mut self, policy: InCodePolicy) -> Self {
        self.options.in_code = policy;
        self
    }

    /// Set the `in_code_mark` policy. Defaults to `Disallow`.
    pub fn in_code_mark(mut self, policy: InCodePolicy) -> Self {
        self.options.in_code_mark = policy;
        self
    }

    /// Borrow the underlying regex. Useful for testing / diagnostics.
    pub fn regex(&self) -> &Regex {
        &self.regex
    }

    /// Borrow the rule's options snapshot.
    pub fn options(&self) -> &RuleOptions {
        &self.options
    }
}

/// Default literal-string handler. Mirrors
/// `prosemirror-inputrules/src/inputrules.ts::stringHandler`.
///
/// If the regex has a capture group, only the captured slice (plus
/// any suffix between the capture group and the end of the full
/// match) is replaced; the prefix before the capture group stays in
/// the document untouched. Without a capture group, the entire
/// match range `[start, end]` is replaced.
fn string_handler(
    replacement: &str,
    state: &EditorState,
    captures: &Captures<'_>,
    start: usize,
    end: usize,
) -> Option<Transaction> {
    let full_match = captures.get(0)?;

    let (insert_text, replace_start) = if let Some(group_one) = captures.get(1) {
        // Replace ONLY the captured slice + the suffix between
        // group-one and full-match end. The prefix before the capture
        // group stays in the document — we shift `replace_start`
        // forward to the capture-group start so `delete(...)` only
        // removes from that point onward.
        let suffix = &full_match.as_str()[group_one.end() - full_match.start()..];
        let mut buf = String::with_capacity(replacement.len() + suffix.len());
        buf.push_str(replacement);
        buf.push_str(suffix);
        let prefix_chars = full_match.as_str()[..group_one.start() - full_match.start()]
            .chars()
            .count();
        (buf, start + prefix_chars)
    } else {
        (replacement.to_string(), start)
    };

    // Use `replace_with` (explicit positions) rather than
    // `insert_text` (selection-relative) — pine's `insert_text`
    // operates on the transaction's selection, while we want to
    // replace exactly `[replace_start, end]` with the inserted
    // text. PM's upstream `tr.insertText(text, from, to)` takes
    // explicit positions; `replace_with` is pine's equivalent.
    //
    // Inherit marks from the END of the input range (the cursor
    // position), NOT from `replace_start`. When the matched range
    // starts at a mark boundary (e.g. unmarked `a ` followed by a
    // strong `-`), resolving at `replace_start` picks up the LEFT
    // side's marks — the unmarked side — and strips formatting
    // from the replacement. Resolving at the cursor instead always
    // sees the active mark span the user is typing inside.
    let marks = state
        .doc()
        .resolve(end)
        .map(|resolved| resolved.marks(state.schema()))
        .unwrap_or_default();
    let mut tr = state.tr();
    let text_node = state.schema().text(insert_text, marks).ok()?;
    tr.replace_with(replace_start, end, Fragment::from(text_node))
        .ok()?;
    Some(tr)
}
