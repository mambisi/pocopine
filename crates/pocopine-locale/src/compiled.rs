use crate::{ArgumentKind, MessageId};

/// A compiler-resolved message reference. Catalog keys are never looked up at
/// runtime. The build identity prevents a template from indexing another
/// application's catalog with a coincidentally valid dense ID.
#[derive(Clone, Copy, Debug)]
pub struct CompiledMessage {
    pub build_id: &'static str,
    pub id: MessageId,
    pub arguments: &'static [(&'static str, ArgumentKind)],
    pub elements: &'static [u16],
    /// Included by generated debug builds only, for visible missing-message
    /// diagnostics. Release templates carry None and retain no key string.
    pub debug_key: Option<&'static str>,
}

impl CompiledMessage {
    /// Match positional template values to the generated signature, whose
    /// argument names are sorted exactly as in the generated Rust functions.
    pub const fn validate(self, arguments: usize, elements: Option<usize>) -> Self {
        assert!(
            self.arguments.len() == arguments,
            "$t argument count does not match the catalog message"
        );
        match elements {
            Some(elements) => {
                assert!(
                    self.elements.len() == elements,
                    "$t child count does not match the catalog element placeholders"
                );
                let mut i = 0;
                while i < self.elements.len() {
                    assert!(
                        self.elements[i] as usize == i,
                        "$t element placeholders must address consecutive source children"
                    );
                    i += 1;
                }
            }
            None => assert!(
                self.elements.is_empty(),
                "rich translation requires a direct $t binding in pp-text"
            ),
        }
        self
    }
}
