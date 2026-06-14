//! The documented utility catalog (RFC 092 D3 — "the catalog *is* the
//! contract") and the LSP/autocomplete metadata derived from it.
//!
//! This is the single description of what Pine Stylekit supports. It
//! renders two ways:
//!
//! - [`Catalog::to_markdown`] — the human/LLM-facing catalog embedded in
//!   `docs/guides/styling/stylekit.md` (regenerated via `pocopine stylekit --docs`).
//! - [`Catalog::to_metadata_json`] — machine-readable completion data for
//!   editors (`pocopine stylekit --metadata`), pairing with the token
//!   manifest ([`crate::ThemeTokens::to_manifest_json`]).
//!
//! It mirrors the families implemented in [`crate::registry`]; keep the
//! two in lockstep when the registry grows.

use serde::Serialize;

/// What kind of value a utility family takes — drives both the docs and
/// editor completion behaviour.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueKind {
    /// No value (`flex`, `truncate`).
    None,
    /// Spacing scale (`p-4`, `gap-2.5`) + keywords (`auto`, `px`, `full`).
    Spacing,
    /// Sizing scale: spacing + `screen`/`full`/named container sizes.
    Size,
    /// A `--color-*` token name (`bg-surface`), `/alpha` modifier allowed.
    Color,
    /// A named scale (`rounded-md`, `shadow-card`, `text-lg`).
    Scale,
    /// A bare number (`opacity-40`, `z-10`, `duration-200`).
    Number,
}

/// One utility family entry.
#[derive(Debug, Clone, Serialize)]
pub struct Family {
    /// `flex`, or a prefix like `p`, `bg`, `text`.
    pub name: &'static str,
    pub value: ValueKind,
    /// Whether `name-[…]` arbitrary values are accepted.
    pub arbitrary: bool,
    pub example: &'static str,
    pub note: &'static str,
}

/// A named group of families.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub title: &'static str,
    pub families: Vec<Family>,
}

/// A documented variant prefix.
#[derive(Debug, Clone, Serialize)]
pub struct VariantDoc {
    pub name: &'static str,
    pub selector: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub groups: Vec<Group>,
    pub variants: Vec<VariantDoc>,
}

/// LSP/autocomplete metadata — a flattened, machine-friendly view.
#[derive(Debug, Serialize)]
pub struct Metadata {
    pub families: Vec<Family>,
    pub variants: Vec<VariantDoc>,
}

fn fam(
    name: &'static str,
    value: ValueKind,
    arbitrary: bool,
    example: &'static str,
    note: &'static str,
) -> Family {
    Family {
        name,
        value,
        arbitrary,
        example,
        note,
    }
}

/// The framework-owned catalog. Mirrors [`crate::registry`].
pub fn catalog() -> Catalog {
    use ValueKind::*;
    Catalog {
        groups: vec![
            Group {
                title: "Display & position",
                families: vec![
                    fam(
                        "flex",
                        None,
                        false,
                        "flex",
                        "also block, inline, inline-block, inline-flex, grid, hidden",
                    ),
                    fam(
                        "relative",
                        None,
                        false,
                        "relative",
                        "also absolute, fixed, sticky, static",
                    ),
                ],
            },
            Group {
                title: "Flexbox & alignment",
                families: vec![
                    fam(
                        "flex-col",
                        None,
                        false,
                        "flex-col",
                        "direction: flex-row/col; wrap: flex-wrap/nowrap; flex-1/none/auto",
                    ),
                    fam(
                        "items-center",
                        None,
                        false,
                        "items-center",
                        "items-/justify- center|start|end|between|around|evenly",
                    ),
                    fam(
                        "shrink",
                        None,
                        false,
                        "shrink-0",
                        "shrink, shrink-0, grow, grow-0",
                    ),
                ],
            },
            Group {
                title: "Spacing",
                families: vec![
                    fam("p", Spacing, false, "p-4", "p/px/py/pt/pb/pl/pr"),
                    fam(
                        "m",
                        Spacing,
                        false,
                        "mx-auto",
                        "m/mx/my/mt/mb/ml/mr (auto allowed)",
                    ),
                    fam("gap", Spacing, false, "gap-2.5", "flex/grid gap"),
                    fam(
                        "space-y",
                        Spacing,
                        false,
                        "space-y-2",
                        "space-y/space-x — margin between children",
                    ),
                ],
            },
            Group {
                title: "Sizing",
                families: vec![
                    fam(
                        "w",
                        Size,
                        true,
                        "w-full",
                        "w, h, size; px/full/screen/named",
                    ),
                    fam(
                        "min-w",
                        Size,
                        true,
                        "min-w-0",
                        "min-w/min-h/max-w/max-h; max-w-{xs..3xl}",
                    ),
                ],
            },
            Group {
                title: "Typography",
                families: vec![
                    fam(
                        "text",
                        Scale,
                        true,
                        "text-[13px]",
                        "named size (text-sm) OR color OR text-center/right",
                    ),
                    fam(
                        "font",
                        Scale,
                        false,
                        "font-medium",
                        "weight (medium/semibold/…) + family (sans/serif/mono)",
                    ),
                    fam(
                        "leading",
                        Scale,
                        true,
                        "leading-relaxed",
                        "tight/snug/normal/relaxed/loose",
                    ),
                    fam("tracking", Scale, true, "tracking-tight", "tighter…widest"),
                    fam(
                        "underline",
                        None,
                        false,
                        "underline",
                        "+ underline-offset-{n}, uppercase, truncate, tabular-nums",
                    ),
                ],
            },
            Group {
                title: "Color (token-backed)",
                families: vec![
                    fam(
                        "bg",
                        Color,
                        true,
                        "bg-surface",
                        "any --color-* token, Tailwind palette (bg-slate-700), transparent/white/black/current",
                    ),
                    fam(
                        "text",
                        Color,
                        false,
                        "text-ink-50",
                        "color when not a size/align keyword",
                    ),
                    fam(
                        "border",
                        Color,
                        false,
                        "border-line",
                        "color when not a width/style keyword",
                    ),
                    fam(
                        "ring",
                        Color,
                        false,
                        "ring-accent",
                        "ring-{n} width, ring-{color} colour",
                    ),
                    fam(
                        "decoration",
                        Color,
                        false,
                        "decoration-1",
                        "thickness (number) or colour",
                    ),
                ],
            },
            Group {
                title: "Borders, radius, shadow",
                families: vec![
                    fam(
                        "border",
                        None,
                        false,
                        "border-b",
                        "border, border-0/2/4, sides t/r/b/l, styles solid/dashed/…",
                    ),
                    fam(
                        "rounded",
                        Scale,
                        true,
                        "rounded-md",
                        "none/sm/md/lg/xl/2xl/3xl/full",
                    ),
                    fam(
                        "shadow",
                        Scale,
                        true,
                        "shadow-card",
                        "sm/md/lg/xl/2xl + any --shadow-* token",
                    ),
                ],
            },
            Group {
                title: "Positioning & effects",
                families: vec![
                    fam(
                        "top",
                        Spacing,
                        true,
                        "top-0",
                        "top/right/bottom/left, inset, inset-x/y",
                    ),
                    fam("z", Number, false, "z-10", "z-index"),
                    fam("opacity", Number, false, "opacity-40", "0–100 → 0–1"),
                    fam("duration", Number, false, "duration-200", "ms"),
                    fam("scale", Number, false, "scale-95", "transform scale"),
                    fam(
                        "backdrop-blur",
                        Scale,
                        false,
                        "backdrop-blur-md",
                        "none/sm/md/lg/xl/2xl/3xl",
                    ),
                    fam(
                        "transition",
                        None,
                        true,
                        "transition-colors",
                        "transition, transition-colors, transition-[props]",
                    ),
                ],
            },
        ],
        variants: vec![
            VariantDoc {
                name: "hover",
                selector: ":hover",
            },
            VariantDoc {
                name: "focus",
                selector: ":focus",
            },
            VariantDoc {
                name: "focus-visible",
                selector: ":focus-visible",
            },
            VariantDoc {
                name: "focus-within",
                selector: ":focus-within",
            },
            VariantDoc {
                name: "active",
                selector: ":active",
            },
            VariantDoc {
                name: "disabled",
                selector: ":disabled",
            },
            VariantDoc {
                name: "checked",
                selector: ":checked",
            },
            VariantDoc {
                name: "first / last",
                selector: ":first-child / :last-child",
            },
            VariantDoc {
                name: "placeholder",
                selector: "::placeholder",
            },
            VariantDoc {
                name: "sm / md / lg / xl / 2xl",
                selector: "@media (min-width: …)",
            },
            VariantDoc {
                name: "data-[k=v]",
                selector: "[data-k=\"v\"]",
            },
            VariantDoc {
                name: "aria-[k=v]",
                selector: "[aria-k=\"v\"]",
            },
        ],
    }
}

impl Catalog {
    /// Render the catalog as Markdown for `docs/guides/styling/stylekit.md`.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        for group in &self.groups {
            s.push_str(&format!("### {}\n\n", group.title));
            s.push_str("| Utility | Value | Arbitrary | Example | Notes |\n");
            s.push_str("|---------|-------|-----------|---------|-------|\n");
            for f in &group.families {
                s.push_str(&format!(
                    "| `{}` | {:?} | {} | `{}` | {} |\n",
                    f.name,
                    f.value,
                    if f.arbitrary { "yes" } else { "—" },
                    f.example,
                    f.note.replace('|', "\\|"),
                ));
            }
            s.push('\n');
        }
        s.push_str("### Variants\n\n| Prefix | Compiles to |\n|--------|-------------|\n");
        for v in &self.variants {
            s.push_str(&format!("| `{}:` | `{}` |\n", v.name, v.selector));
        }
        s.push('\n');
        s
    }

    /// Flatten to LSP/autocomplete [`Metadata`] and serialise as JSON.
    pub fn to_metadata_json(&self) -> String {
        let meta = Metadata {
            families: self
                .groups
                .iter()
                .flat_map(|g| g.families.clone())
                .collect(),
            variants: self.variants.clone(),
        };
        serde_json::to_string_pretty(&meta).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_lists_core_families() {
        let md = catalog().to_markdown();
        assert!(md.contains("### Spacing"));
        assert!(md.contains("`bg`"));
        assert!(md.contains("| `hover:` | `:hover` |"));
    }

    #[test]
    fn metadata_is_valid_json_with_families() {
        let json = catalog().to_metadata_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["families"].as_array().unwrap().len() > 10);
        assert!(v["variants"].as_array().unwrap().len() > 5);
    }
}
