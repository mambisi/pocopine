//! Stable observability event contract shared by pocopine logging,
//! tracing, telemetry, and analytics integrations.
//!
//! Core/runtime crates should emit `tracing` spans and events, or
//! construct an [`ObservedEvent`] when they need a stable public schema.
//! Exporters live in `pocopine-logging` and `pocopine-analytics`.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use tracing::Level;

pub const LOG_TARGET: &str = "pocopine.log";
pub const TRACE_TARGET: &str = "pocopine.trace";
pub const METRIC_TARGET: &str = "pocopine.metric";
pub const ANALYTICS_TARGET: &str = "pocopine.analytics";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventClass {
    Log,
    Trace,
    Metric,
    Analytics,
}

impl EventClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Trace => "trace",
            Self::Metric => "metric",
            Self::Analytics => "analytics",
        }
    }

    pub fn target(self) -> &'static str {
        match self {
            Self::Log => LOG_TARGET,
            Self::Trace => TRACE_TARGET,
            Self::Metric => METRIC_TARGET,
            Self::Analytics => ANALYTICS_TARGET,
        }
    }
}

impl fmt::Display for EventClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPriority {
    Critical,
    High,
    Normal,
    Low,
}

impl EventPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    pub fn level(self) -> Level {
        match self {
            Self::Critical => Level::ERROR,
            Self::High => Level::WARN,
            Self::Normal => Level::INFO,
            Self::Low => Level::DEBUG,
        }
    }
}

impl fmt::Display for EventPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldPrivacy {
    Public,
    Pseudonymous,
    Sensitive,
}

impl FieldPrivacy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Pseudonymous => "pseudonymous",
            Self::Sensitive => "sensitive",
        }
    }
}

impl fmt::Display for FieldPrivacy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    String(String),
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
}

impl FieldValue {
    pub fn as_debug_value(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::I64(value) => value.to_string(),
            Self::U64(value) => value.to_string(),
            Self::F64(value) => value.to_string(),
        }
    }
}

impl From<&str> for FieldValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for FieldValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for FieldValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for FieldValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<i32> for FieldValue {
    fn from(value: i32) -> Self {
        Self::I64(value.into())
    }
}

impl From<u64> for FieldValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<u32> for FieldValue {
    fn from(value: u32) -> Self {
        Self::U64(value.into())
    }
}

impl From<f64> for FieldValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<f32> for FieldValue {
    fn from(value: f32) -> Self {
        Self::F64(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservedField {
    pub value: FieldValue,
    pub privacy: FieldPrivacy,
}

impl ObservedField {
    pub fn new(value: impl Into<FieldValue>, privacy: FieldPrivacy) -> Self {
        Self {
            value: value.into(),
            privacy,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObserveContext {
    pub service: Option<String>,
    pub environment: Option<String>,
    pub route: Option<String>,
    pub component: Option<String>,
    pub trace_id: Option<String>,
    pub session_id: Option<String>,
    pub user_id_hash: Option<String>,
}

impl ObserveContext {
    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = Some(service.into());
        self
    }

    pub fn with_environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.route = Some(route.into());
        self
    }

    pub fn with_component(mut self, component: impl Into<String>) -> Self {
        self.component = Some(component.into());
        self
    }

    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_user_id_hash(mut self, user_id_hash: impl Into<String>) -> Self {
        self.user_id_hash = Some(user_id_hash.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservedEvent {
    pub name: String,
    pub version: u16,
    pub class: EventClass,
    pub priority: EventPriority,
    pub privacy: FieldPrivacy,
    pub context: ObserveContext,
    pub fields: BTreeMap<String, ObservedField>,
}

impl ObservedEvent {
    pub fn new(name: impl Into<String>, class: EventClass) -> Self {
        Self {
            name: name.into(),
            version: 1,
            class,
            priority: EventPriority::Normal,
            privacy: FieldPrivacy::Public,
            context: ObserveContext::default(),
            fields: BTreeMap::new(),
        }
    }

    pub fn log(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Log)
    }

    pub fn trace(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Trace)
    }

    pub fn metric(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Metric)
    }

    pub fn analytics(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Analytics).privacy(FieldPrivacy::Pseudonymous)
    }

    pub fn version(mut self, version: u16) -> Self {
        self.version = version;
        self
    }

    pub fn priority(mut self, priority: EventPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn privacy(mut self, privacy: FieldPrivacy) -> Self {
        self.privacy = privacy;
        self
    }

    pub fn context(mut self, context: ObserveContext) -> Self {
        self.context = context;
        self
    }

    pub fn field(
        mut self,
        name: impl Into<String>,
        value: impl Into<FieldValue>,
        privacy: FieldPrivacy,
    ) -> Self {
        self.insert_field(name, value, privacy);
        self
    }

    pub fn insert_field(
        &mut self,
        name: impl Into<String>,
        value: impl Into<FieldValue>,
        privacy: FieldPrivacy,
    ) {
        self.fields
            .insert(name.into(), ObservedField::new(value, privacy));
    }

    pub fn redacted(&self, policy: RedactionPolicy) -> Self {
        let mut event = self.clone();
        event.fields.retain(|_, field| policy.allows(field.privacy));
        if !policy.include_pseudonymous {
            event.context.session_id = None;
            event.context.user_id_hash = None;
        }
        event
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedactionPolicy {
    pub include_pseudonymous: bool,
    pub include_sensitive: bool,
}

impl RedactionPolicy {
    pub const fn public_only() -> Self {
        Self {
            include_pseudonymous: false,
            include_sensitive: false,
        }
    }

    pub const fn allow_pseudonymous() -> Self {
        Self {
            include_pseudonymous: true,
            include_sensitive: false,
        }
    }

    pub const fn allow_sensitive() -> Self {
        Self {
            include_pseudonymous: true,
            include_sensitive: true,
        }
    }

    pub fn allows(self, privacy: FieldPrivacy) -> bool {
        match privacy {
            FieldPrivacy::Public => true,
            FieldPrivacy::Pseudonymous => self.include_pseudonymous,
            FieldPrivacy::Sensitive => self.include_sensitive,
        }
    }
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self::public_only()
    }
}

pub fn emit_tracing(event: &ObservedEvent) {
    macro_rules! emit_for_target {
        ($target:literal, $event:expr) => {
            match $event.priority.level() {
                Level::ERROR => tracing::event!(
                    target: $target,
                    Level::ERROR,
                    event_name = %$event.name,
                    event_version = $event.version,
                    event_class = %$event.class,
                    event_priority = %$event.priority,
                    event_privacy = %$event.privacy,
                    context = ?$event.context,
                    fields = ?$event.fields,
                ),
                Level::WARN => tracing::event!(
                    target: $target,
                    Level::WARN,
                    event_name = %$event.name,
                    event_version = $event.version,
                    event_class = %$event.class,
                    event_priority = %$event.priority,
                    event_privacy = %$event.privacy,
                    context = ?$event.context,
                    fields = ?$event.fields,
                ),
                Level::INFO => tracing::event!(
                    target: $target,
                    Level::INFO,
                    event_name = %$event.name,
                    event_version = $event.version,
                    event_class = %$event.class,
                    event_priority = %$event.priority,
                    event_privacy = %$event.privacy,
                    context = ?$event.context,
                    fields = ?$event.fields,
                ),
                Level::DEBUG | Level::TRACE => tracing::event!(
                    target: $target,
                    Level::DEBUG,
                    event_name = %$event.name,
                    event_version = $event.version,
                    event_class = %$event.class,
                    event_priority = %$event.priority,
                    event_privacy = %$event.privacy,
                    context = ?$event.context,
                    fields = ?$event.fields,
                ),
            }
        };
    }

    match event.class {
        EventClass::Log => emit_for_target!("pocopine.log", event),
        EventClass::Trace => emit_for_target!("pocopine.trace", event),
        EventClass::Metric => emit_for_target!("pocopine.metric", event),
        EventClass::Analytics => emit_for_target!("pocopine.analytics", event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_non_public_fields_by_default() {
        let event = ObservedEvent::analytics("checkout")
            .field("plan", "pro", FieldPrivacy::Public)
            .field("session", "s-1", FieldPrivacy::Pseudonymous)
            .field("email", "person@example.test", FieldPrivacy::Sensitive);

        let redacted = event.redacted(RedactionPolicy::public_only());

        assert!(redacted.fields.contains_key("plan"));
        assert!(!redacted.fields.contains_key("session"));
        assert!(!redacted.fields.contains_key("email"));
    }

    #[test]
    fn can_allow_pseudonymous_without_sensitive_fields() {
        let context = ObserveContext::default().with_session_id("s-1");
        let event = ObservedEvent::analytics("route_view")
            .context(context)
            .field("route", "/settings", FieldPrivacy::Public)
            .field("session", "s-1", FieldPrivacy::Pseudonymous)
            .field("token", "secret", FieldPrivacy::Sensitive);

        let redacted = event.redacted(RedactionPolicy::allow_pseudonymous());

        assert_eq!(redacted.context.session_id.as_deref(), Some("s-1"));
        assert!(redacted.fields.contains_key("route"));
        assert!(redacted.fields.contains_key("session"));
        assert!(!redacted.fields.contains_key("token"));
    }
}
