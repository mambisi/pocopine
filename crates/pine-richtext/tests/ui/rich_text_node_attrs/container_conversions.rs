use pine_richtext::RichTextNodeAttrs;

#[derive(RichTextNodeAttrs)]
#[serde(try_from = "LegacyAttrs")]
struct TryFromAttrs {
    label: String,
}

#[derive(RichTextNodeAttrs)]
#[serde(into = "LegacyAttrs")]
struct IntoAttrs {
    label: String,
}

struct LegacyAttrs;

fn main() {}
