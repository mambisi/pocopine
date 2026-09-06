use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{FormatError, Locale, Message, MessageError, MessagePart, Value};

pub const CATALOG_FORMAT_VERSION: u16 = 1;
pub(crate) const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_MESSAGES: usize = 100_000;

/// A build-local positional message index. Never persist this in a durable
/// job or use it across builds; generated call sites and catalogs are paired.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MessageId(pub u32);

/// Prevent host-only catalog contents from being installed as browser data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogAudience {
    Browser,
    Host,
}

/// Compiler output for one positional slot. A fallback retains the language
/// of its text so its plural grammar does not change to the requested locale.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub source_locale: Locale,
    pub message: String,
}

/// Versioned wire artifact. Constructing this type does not validate it;
/// only [`Catalog::load`] produces an installable immutable catalog.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogArtifact {
    pub format_version: u16,
    pub build_id: String,
    pub locale: Locale,
    pub audience: CatalogAudience,
    /// All target catalogs retain the same indices. Unreachable entries are
    /// null, not removed, so pruning cannot silently shift another message.
    pub messages: Vec<Option<CatalogEntry>>,
}

/// Identity emitted alongside generated code and artifact URL metadata.
/// All fields are checked before a catalog can be used for translation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogIdentity {
    build_id: String,
    locale: Locale,
    audience: CatalogAudience,
    message_count: usize,
}

impl CatalogIdentity {
    pub fn new(
        build_id: String,
        locale: Locale,
        audience: CatalogAudience,
        message_count: usize,
    ) -> Result<Self, CatalogError> {
        if build_id.len() != 64
            || !build_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(CatalogError::InvalidIdentity(
                "build ID must be a lowercase SHA-256 digest".into(),
            ));
        }
        if message_count > MAX_MESSAGES {
            return Err(CatalogError::InvalidIdentity(
                "catalog exceeds 100000 messages".into(),
            ));
        }
        Ok(Self {
            build_id,
            locale,
            audience,
            message_count,
        })
    }
    pub fn build_id(&self) -> &str {
        &self.build_id
    }
    pub fn locale(&self) -> &Locale {
        &self.locale
    }
    pub fn audience(&self) -> CatalogAudience {
        self.audience
    }
    pub fn message_count(&self) -> usize {
        self.message_count
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    InvalidIdentity(String),
    TooLarge,
    Decode(String),
    FormatVersion(u16),
    BuildMismatch,
    LocaleMismatch,
    AudienceMismatch,
    MessageCountMismatch,
    InvalidMessage { id: MessageId, error: MessageError },
    MissingMessage(MessageId),
    Format(FormatError),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity(s) => write!(f, "invalid catalog identity: {s}"),
            Self::TooLarge => f.write_str("catalog exceeds 16 MiB"),
            Self::Decode(s) => write!(f, "invalid catalog: {s}"),
            Self::FormatVersion(v) => write!(f, "unsupported catalog format version {v}"),
            Self::BuildMismatch => f.write_str("catalog build ID does not match the running code"),
            Self::LocaleMismatch => {
                f.write_str("catalog locale does not match the requested locale")
            }
            Self::AudienceMismatch => f.write_str("catalog audience does not match its consumer"),
            Self::MessageCountMismatch => {
                f.write_str("catalog message count does not match generated code")
            }
            Self::InvalidMessage { id, error } => write!(f, "invalid message {}: {error}", id.0),
            Self::MissingMessage(id) => {
                write!(f, "message {} is not present in this catalog", id.0)
            }
            Self::Format(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for CatalogError {}

#[derive(Clone, Debug)]
pub struct CatalogMessage {
    source_locale: Locale,
    message: Message,
}

impl CatalogMessage {
    pub fn source_locale(&self) -> &Locale {
        &self.source_locale
    }
    pub fn message(&self) -> &Message {
        &self.message
    }
}

/// Fully validated, immutable translation data. Share it across requests or
/// worker jobs; it stores no current locale and does no per-request parsing.
#[derive(Clone, Debug)]
pub struct Catalog {
    identity: CatalogIdentity,
    messages: Vec<Option<CatalogMessage>>,
}

impl Catalog {
    pub fn load(bytes: &[u8], expected: &CatalogIdentity) -> Result<Self, CatalogError> {
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(CatalogError::TooLarge);
        }
        let artifact: CatalogArtifact =
            serde_json::from_slice(bytes).map_err(|e| CatalogError::Decode(e.to_string()))?;
        if artifact.format_version != CATALOG_FORMAT_VERSION {
            return Err(CatalogError::FormatVersion(artifact.format_version));
        }
        if artifact.build_id != expected.build_id {
            return Err(CatalogError::BuildMismatch);
        }
        if artifact.locale != expected.locale {
            return Err(CatalogError::LocaleMismatch);
        }
        if artifact.audience != expected.audience {
            return Err(CatalogError::AudienceMismatch);
        }
        if artifact.messages.len() != expected.message_count {
            return Err(CatalogError::MessageCountMismatch);
        }
        let messages = artifact
            .messages
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                entry
                    .map(|entry| {
                        let message = Message::parse(&entry.message).map_err(|error| {
                            CatalogError::InvalidMessage {
                                id: MessageId(index as u32),
                                error,
                            }
                        })?;
                        Ok(CatalogMessage {
                            source_locale: entry.source_locale,
                            message,
                        })
                    })
                    .transpose()
            })
            .collect::<Result<_, CatalogError>>()?;
        Ok(Self {
            identity: expected.clone(),
            messages,
        })
    }

    pub fn identity(&self) -> &CatalogIdentity {
        &self.identity
    }

    pub fn message(&self, id: MessageId) -> Result<&CatalogMessage, CatalogError> {
        self.messages
            .get(id.0 as usize)
            .and_then(Option::as_ref)
            .ok_or(CatalogError::MissingMessage(id))
    }

    pub fn parts<'a>(
        &'a self,
        id: MessageId,
        args: &'a [(&str, Value<'a>)],
    ) -> Result<Vec<MessagePart<'a>>, CatalogError> {
        let message = self.message(id)?;
        message
            .message
            .parts(&message.source_locale, args)
            .map_err(CatalogError::Format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (CatalogArtifact, CatalogIdentity) {
        let build_id = "a".repeat(64);
        let artifact = CatalogArtifact {
            format_version: CATALOG_FORMAT_VERSION,
            build_id: build_id.clone(),
            locale: "fr".parse().unwrap(),
            audience: CatalogAudience::Browser,
            messages: vec![
                Some(CatalogEntry {
                    source_locale: "en".parse().unwrap(),
                    message: "{n, plural, one {one item} other {many items}}".into(),
                }),
                None,
            ],
        };
        let identity = CatalogIdentity::new(
            build_id,
            artifact.locale.clone(),
            artifact.audience,
            artifact.messages.len(),
        )
        .unwrap();
        (artifact, identity)
    }

    fn load(
        artifact: &CatalogArtifact,
        identity: &CatalogIdentity,
    ) -> Result<Catalog, CatalogError> {
        Catalog::load(&serde_json::to_vec(artifact).unwrap(), identity)
    }

    #[test]
    fn fallback_text_keeps_its_own_plural_grammar_and_pruned_slots_do_not_shift() {
        let (artifact, identity) = fixture();
        let catalog = load(&artifact, &identity).unwrap();
        let args = [("n", Value::Number(0u64.into()))];
        assert_eq!(
            catalog.parts(MessageId(0), &args).unwrap(),
            vec![MessagePart::Text("many items".into())]
        );
        assert_eq!(
            catalog.message(MessageId(1)).unwrap_err(),
            CatalogError::MissingMessage(MessageId(1))
        );
        assert!(catalog.message(MessageId(u32::MAX)).is_err());
    }

    #[test]
    fn refuses_stale_wrong_target_wrong_locale_and_incomplete_artifacts() {
        let (artifact, identity) = fixture();
        let mut changed = artifact.clone();
        changed.build_id = "b".repeat(64);
        assert!(matches!(
            load(&changed, &identity),
            Err(CatalogError::BuildMismatch)
        ));
        let mut changed = artifact.clone();
        changed.audience = CatalogAudience::Host;
        assert!(matches!(
            load(&changed, &identity),
            Err(CatalogError::AudienceMismatch)
        ));
        let mut changed = artifact.clone();
        changed.locale = "en".parse().unwrap();
        assert!(matches!(
            load(&changed, &identity),
            Err(CatalogError::LocaleMismatch)
        ));
        let mut changed = artifact.clone();
        changed.messages.pop();
        assert!(matches!(
            load(&changed, &identity),
            Err(CatalogError::MessageCountMismatch)
        ));
        let mut changed = artifact.clone();
        changed.format_version += 1;
        assert!(matches!(
            load(&changed, &identity),
            Err(CatalogError::FormatVersion(_))
        ));
        let mut changed = artifact;
        changed.messages[0].as_mut().unwrap().message = "{broken".into();
        assert!(matches!(
            load(&changed, &identity),
            Err(CatalogError::InvalidMessage { .. })
        ));
    }

    #[test]
    fn immutable_catalogs_can_be_shared_across_concurrent_consumers() {
        let (artifact, identity) = fixture();
        let catalog = std::sync::Arc::new(load(&artifact, &identity).unwrap());
        std::thread::scope(|scope| {
            for n in 0u64..16 {
                let catalog = catalog.clone();
                scope.spawn(move || {
                    let args = [("n", Value::Number(n.into()))];
                    let expected = if n == 1 { "one item" } else { "many items" };
                    assert_eq!(
                        catalog.parts(MessageId(0), &args).unwrap(),
                        vec![MessagePart::Text(expected.into())]
                    );
                });
            }
        });
    }
}
