//! RFC-116 — `poco!` inline templates.
//!
//! These run on the host: they assert what the macro *expands to*, which is
//! where the interesting behaviour lives (verbatim recovery, quoted-text
//! unquoting). Mount behaviour is covered by the existing wasm suites, which
//! see an inline template as an ordinary registered template.

use pocopine::PocoTemplate;
use pocopine::{component, handlers, poco};
use serde::{Deserialize, Serialize};

// The desugar path: `template = poco! { … }` must feed the same pipeline a
// file template does, so this fixture exercises validation, plan
// compilation and the RFC-111 path assertions in one declaration. If any
// pass rejected the inline form, this would not compile.
#[derive(Default, Serialize, Deserialize)]
#[component(name = "poco-inline-fixture", template = poco! {
    <div class="fixture">
        <span pp-text="label"></span>
        <button pp-on:click="bump">{{ count }}</button>
        <p>"Don't stop — © 2026"</p>
    </div>
})]
struct InlineFixture {
    #[prop]
    label: String,
    #[prop]
    count: i32,
}

#[handlers]
impl InlineFixture {
    fn bump(&mut self) {
        self.count += 1;
    }
}

#[test]
fn component_accepts_a_poco_template() {
    // Reaching this point means the attribute form expanded: the template
    // parsed, `label`/`count` resolved against the struct's fields, and the
    // quoted run was unquoted before validation saw it.
    let fixture = InlineFixture::default();
    assert_eq!(fixture.count, 0);
}

#[test]
fn recovers_body_verbatim_including_whitespace_and_sugar() {
    const CARD: PocoTemplate = poco! {
        <div class="card" :title="x" @click="dismiss" pp-if="ready">
            {{ count }}
            <span pp-text="title">Hello</span>
            <a href="/docs/guide#top" pp-on:click.debounce.300="go">Link &amp; more</a>
            <!-- an HTML comment -->
            <input type="text" pp-model.number="qty" disabled />
            <br/>
        </div>
    };

    let html = CARD.as_str();
    // Attributes, sugar and structure survive byte-for-byte.
    assert!(
        html.starts_with("<div class=\"card\" :title=\"x\" @click=\"dismiss\" pp-if=\"ready\">")
    );
    assert!(html.contains("{{ count }}"));
    assert!(html.contains("<span pp-text=\"title\">Hello</span>"));
    assert!(html.contains("pp-on:click.debounce.300=\"go\""));
    assert!(html.contains("<!-- an HTML comment -->"));
    assert!(html.contains("pp-model.number=\"qty\""));
    assert!(html.trim_end().ends_with("</div>"));
    // Indentation is preserved — the source IS the artifact.
    assert!(html.contains("\n            <span"));
}

#[test]
fn quoted_text_is_unquoted_into_static_text() {
    // Every one of these characters makes the Rust lexer reject a bare
    // token body; quoting turns the run into a single opaque token.
    const COPY: PocoTemplate = poco! {
        <p>"Don't stop — © 2026 · ⌘K 🎉"</p>
    };

    assert_eq!(COPY.as_str(), "<p>Don't stop — © 2026 · ⌘K 🎉</p>");
}

#[test]
fn quoted_text_is_html_escaped() {
    const ESCAPED: PocoTemplate = poco! {
        <p>"5 < 10 & rising"</p>
    };

    // The author wrote text, so `<` and `&` become entities rather than
    // corrupting the markup.
    assert_eq!(ESCAPED.as_str(), "<p>5 &lt; 10 &amp; rising</p>");
}

#[test]
fn quoted_and_plain_text_mix_within_one_node() {
    const MIXED: PocoTemplate = poco! {
        <p>Hello "don't" world</p>
    };

    assert_eq!(MIXED.as_str(), "<p>Hello don't world</p>");
}

#[test]
fn attribute_values_keep_their_quotes() {
    const ATTRS: PocoTemplate = poco! {
        <a href="/x" :title="y" class="a b">"…"</a>
    };

    // Attribute literals are left verbatim; only the text run is unquoted.
    assert_eq!(
        ATTRS.as_str(),
        "<a href=\"/x\" :title=\"y\" class=\"a b\">…</a>"
    );
}

#[test]
fn literals_inside_interpolation_are_left_to_the_expression_parser() {
    const INTERP: PocoTemplate = poco! {
        <p>{{ "Don't stop" }}</p>
    };

    // `{{ }}` is expression territory — the quotes must survive so
    // pocopine-expr sees a string literal.
    assert_eq!(INTERP.as_str(), "<p>{{ \"Don't stop\" }}</p>");
}

#[test]
fn fragments_are_allowed_standalone() {
    // The single-root rule belongs to `#[component]`, not to the macro:
    // a standalone template may be a fragment.
    const FRAGMENT: PocoTemplate = poco! {
        <li>a</li>
        <li>b</li>
    };

    assert!(FRAGMENT.as_str().contains("<li>a</li>"));
    assert!(FRAGMENT.as_str().contains("<li>b</li>"));
}

#[test]
fn poco_template_behaves_like_a_str() {
    const T: PocoTemplate = poco! { <div>x</div> };

    assert_eq!(T.as_str(), "<div>x</div>");
    assert_eq!(T.to_string(), "<div>x</div>");
    assert!(T.starts_with("<div>"));
    let as_ref: &str = T.as_ref();
    assert_eq!(as_ref, "<div>x</div>");
}
