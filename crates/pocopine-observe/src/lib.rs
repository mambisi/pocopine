//! Stable observability event contract shared by pocopine logging,
//! tracing, telemetry, and analytics integrations.
//!
//! Core/runtime crates should emit `tracing` spans and events, or
//! construct an [`ObservedEvent`] when they need a stable public schema.
//! Exporters live in `pocopine-logging` and `pocopine-analytics`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

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

const OBSERVED_TRACING_FIELD_SLOTS: usize = 8;
static OBSERVED_TRACING_OVERFLOW_WARNED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
struct ObservedTracingField<'a> {
    present: bool,
    name: &'a str,
    privacy: &'static str,
    kind: &'static str,
    value_string: &'a str,
    value_bool: bool,
    value_i64: i64,
    value_u64: u64,
    value_f64: f64,
}

impl<'a> ObservedTracingField<'a> {
    const EMPTY: Self = Self {
        present: false,
        name: "",
        privacy: "",
        kind: "",
        value_string: "",
        value_bool: false,
        value_i64: 0,
        value_u64: 0,
        value_f64: 0.0,
    };

    fn from_field(name: &'a str, field: &'a ObservedField) -> Self {
        let mut slot = Self {
            present: true,
            name,
            privacy: field.privacy.as_str(),
            ..Self::EMPTY
        };

        match &field.value {
            FieldValue::String(value) => {
                slot.kind = "string";
                slot.value_string = value;
            }
            FieldValue::Bool(value) => {
                slot.kind = "bool";
                slot.value_bool = *value;
            }
            FieldValue::I64(value) => {
                slot.kind = "i64";
                slot.value_i64 = *value;
            }
            FieldValue::U64(value) => {
                slot.kind = "u64";
                slot.value_u64 = *value;
            }
            FieldValue::F64(value) => {
                slot.kind = "f64";
                slot.value_f64 = *value;
            }
        }

        slot
    }
}

#[derive(Clone, Debug)]
struct ObservedTracing<'a> {
    event: &'a ObservedEvent,
    fields: [ObservedTracingField<'a>; OBSERVED_TRACING_FIELD_SLOTS],
    field_count: u64,
    field_overflowed: bool,
}

impl<'a> ObservedTracing<'a> {
    fn new(event: &'a ObservedEvent) -> Self {
        let mut fields = [ObservedTracingField::EMPTY; OBSERVED_TRACING_FIELD_SLOTS];

        for (slot, (name, field)) in fields.iter_mut().zip(event.fields.iter()) {
            *slot = ObservedTracingField::from_field(name, field);
        }

        Self {
            event,
            fields,
            field_count: event.fields.len() as u64,
            field_overflowed: event.fields.len() > OBSERVED_TRACING_FIELD_SLOTS,
        }
    }

    fn event_name(&self) -> &str {
        &self.event.name
    }

    fn event_class(&self) -> &'static str {
        self.event.class.as_str()
    }

    fn event_priority(&self) -> &'static str {
        self.event.priority.as_str()
    }

    fn event_privacy(&self) -> &'static str {
        self.event.privacy.as_str()
    }

    fn service(&self) -> &str {
        self.event.context.service.as_deref().unwrap_or("")
    }

    fn has_service(&self) -> bool {
        self.event.context.service.is_some()
    }

    fn environment(&self) -> &str {
        self.event.context.environment.as_deref().unwrap_or("")
    }

    fn has_environment(&self) -> bool {
        self.event.context.environment.is_some()
    }

    fn route(&self) -> &str {
        self.event.context.route.as_deref().unwrap_or("")
    }

    fn has_route(&self) -> bool {
        self.event.context.route.is_some()
    }

    fn component(&self) -> &str {
        self.event.context.component.as_deref().unwrap_or("")
    }

    fn has_component(&self) -> bool {
        self.event.context.component.is_some()
    }

    fn trace_id(&self) -> &str {
        self.event.context.trace_id.as_deref().unwrap_or("")
    }

    fn has_trace_id(&self) -> bool {
        self.event.context.trace_id.is_some()
    }

    fn session_id(&self) -> &str {
        self.event.context.session_id.as_deref().unwrap_or("")
    }

    fn has_session_id(&self) -> bool {
        self.event.context.session_id.is_some()
    }

    fn user_id_hash(&self) -> &str {
        self.event.context.user_id_hash.as_deref().unwrap_or("")
    }

    fn has_user_id_hash(&self) -> bool {
        self.event.context.user_id_hash.is_some()
    }

    fn slot(&self, index: usize) -> ObservedTracingField<'a> {
        self.fields[index]
    }
}

/// Emits an observed event into `tracing`.
///
/// This function preserves the event exactly as supplied. It does not apply
/// redaction; call [`ObservedEvent::redacted`] first, or emit through
/// `pocopine-analytics`, when private fields must be stripped before export.
pub fn emit_tracing(event: &ObservedEvent) {
    let observed = ObservedTracing::new(event);

    if observed.field_overflowed && !OBSERVED_TRACING_OVERFLOW_WARNED.swap(true, Ordering::Relaxed)
    {
        tracing::warn!(
            target: "pocopine.log",
            event_name = observed.event_name(),
            observed_field_count = observed.field_count,
            observed_field_slots = OBSERVED_TRACING_FIELD_SLOTS as u64,
            "observed event field slots overflowed; extra fields omitted from tracing output"
        );
    }

    macro_rules! emit_at_level {
        ($target:literal, $level:expr, $observed:expr) => {{
            let f0 = $observed.slot(0);
            let f1 = $observed.slot(1);
            let f2 = $observed.slot(2);
            let f3 = $observed.slot(3);
            let f4 = $observed.slot(4);
            let f5 = $observed.slot(5);
            let f6 = $observed.slot(6);
            let f7 = $observed.slot(7);

            tracing::event!(
                target: $target,
                $level,
                event_name = $observed.event_name(),
                event_version = $observed.event.version,
                event_class = $observed.event_class(),
                event_priority = $observed.event_priority(),
                event_privacy = $observed.event_privacy(),
                observed_context_has_service = $observed.has_service(),
                observed_context_service = $observed.service(),
                observed_context_has_environment = $observed.has_environment(),
                observed_context_environment = $observed.environment(),
                observed_context_has_route = $observed.has_route(),
                observed_context_route = $observed.route(),
                observed_context_has_component = $observed.has_component(),
                observed_context_component = $observed.component(),
                observed_context_has_trace_id = $observed.has_trace_id(),
                observed_context_trace_id = $observed.trace_id(),
                observed_context_has_session_id = $observed.has_session_id(),
                observed_context_session_id = $observed.session_id(),
                observed_context_has_user_id_hash = $observed.has_user_id_hash(),
                observed_context_user_id_hash = $observed.user_id_hash(),
                observed_field_slots = OBSERVED_TRACING_FIELD_SLOTS as u64,
                observed_field_count = $observed.field_count,
                observed_field_overflowed = $observed.field_overflowed,
                observed_field_0_present = f0.present,
                observed_field_0_name = f0.name,
                observed_field_0_privacy = f0.privacy,
                observed_field_0_kind = f0.kind,
                observed_field_0_value_string = f0.value_string,
                observed_field_0_value_bool = f0.value_bool,
                observed_field_0_value_i64 = f0.value_i64,
                observed_field_0_value_u64 = f0.value_u64,
                observed_field_0_value_f64 = f0.value_f64,
                observed_field_1_present = f1.present,
                observed_field_1_name = f1.name,
                observed_field_1_privacy = f1.privacy,
                observed_field_1_kind = f1.kind,
                observed_field_1_value_string = f1.value_string,
                observed_field_1_value_bool = f1.value_bool,
                observed_field_1_value_i64 = f1.value_i64,
                observed_field_1_value_u64 = f1.value_u64,
                observed_field_1_value_f64 = f1.value_f64,
                observed_field_2_present = f2.present,
                observed_field_2_name = f2.name,
                observed_field_2_privacy = f2.privacy,
                observed_field_2_kind = f2.kind,
                observed_field_2_value_string = f2.value_string,
                observed_field_2_value_bool = f2.value_bool,
                observed_field_2_value_i64 = f2.value_i64,
                observed_field_2_value_u64 = f2.value_u64,
                observed_field_2_value_f64 = f2.value_f64,
                observed_field_3_present = f3.present,
                observed_field_3_name = f3.name,
                observed_field_3_privacy = f3.privacy,
                observed_field_3_kind = f3.kind,
                observed_field_3_value_string = f3.value_string,
                observed_field_3_value_bool = f3.value_bool,
                observed_field_3_value_i64 = f3.value_i64,
                observed_field_3_value_u64 = f3.value_u64,
                observed_field_3_value_f64 = f3.value_f64,
                observed_field_4_present = f4.present,
                observed_field_4_name = f4.name,
                observed_field_4_privacy = f4.privacy,
                observed_field_4_kind = f4.kind,
                observed_field_4_value_string = f4.value_string,
                observed_field_4_value_bool = f4.value_bool,
                observed_field_4_value_i64 = f4.value_i64,
                observed_field_4_value_u64 = f4.value_u64,
                observed_field_4_value_f64 = f4.value_f64,
                observed_field_5_present = f5.present,
                observed_field_5_name = f5.name,
                observed_field_5_privacy = f5.privacy,
                observed_field_5_kind = f5.kind,
                observed_field_5_value_string = f5.value_string,
                observed_field_5_value_bool = f5.value_bool,
                observed_field_5_value_i64 = f5.value_i64,
                observed_field_5_value_u64 = f5.value_u64,
                observed_field_5_value_f64 = f5.value_f64,
                observed_field_6_present = f6.present,
                observed_field_6_name = f6.name,
                observed_field_6_privacy = f6.privacy,
                observed_field_6_kind = f6.kind,
                observed_field_6_value_string = f6.value_string,
                observed_field_6_value_bool = f6.value_bool,
                observed_field_6_value_i64 = f6.value_i64,
                observed_field_6_value_u64 = f6.value_u64,
                observed_field_6_value_f64 = f6.value_f64,
                observed_field_7_present = f7.present,
                observed_field_7_name = f7.name,
                observed_field_7_privacy = f7.privacy,
                observed_field_7_kind = f7.kind,
                observed_field_7_value_string = f7.value_string,
                observed_field_7_value_bool = f7.value_bool,
                observed_field_7_value_i64 = f7.value_i64,
                observed_field_7_value_u64 = f7.value_u64,
                observed_field_7_value_f64 = f7.value_f64,
            );
        }};
    }

    macro_rules! emit_for_target {
        ($target:literal, $observed:expr) => {
            match $observed.event.priority.level() {
                Level::ERROR => emit_at_level!($target, Level::ERROR, $observed),
                Level::WARN => emit_at_level!($target, Level::WARN, $observed),
                Level::INFO => emit_at_level!($target, Level::INFO, $observed),
                Level::DEBUG | Level::TRACE => emit_at_level!($target, Level::DEBUG, $observed),
            }
        };
    }

    match event.class {
        EventClass::Log => emit_for_target!("pocopine.log", observed),
        EventClass::Trace => emit_for_target!("pocopine.trace", observed),
        EventClass::Metric => emit_for_target!("pocopine.metric", observed),
        EventClass::Analytics => emit_for_target!("pocopine.analytics", observed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Layer;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("test writer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedWriter {
        type Writer = TestWriter;

        fn make_writer(&'a self) -> Self::Writer {
            TestWriter(Arc::clone(&self.0))
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum CapturedValue {
        Bool(bool),
        I64(i64),
        U64(u64),
        F64(f64),
        Str(String),
        Debug(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    struct CapturedEvent {
        target: String,
        fields: BTreeMap<String, CapturedValue>,
    }

    #[derive(Clone, Default)]
    struct CaptureLayer {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = CaptureVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("captured events lock")
                .push(CapturedEvent {
                    target: event.metadata().target().to_string(),
                    fields: visitor.fields,
                });
        }
    }

    #[derive(Default)]
    struct CaptureVisitor {
        fields: BTreeMap<String, CapturedValue>,
    }

    impl Visit for CaptureVisitor {
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::Bool(value));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::I64(value));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::U64(value));
        }

        fn record_f64(&mut self, field: &Field, value: f64) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::F64(value));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields.insert(
                field.name().to_string(),
                CapturedValue::Str(value.to_string()),
            );
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields.insert(
                field.name().to_string(),
                CapturedValue::Debug(format!("{value:?}")),
            );
        }
    }

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

    #[test]
    fn emit_tracing_json_output_uses_structured_observed_fields() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_writer(SharedWriter(Arc::clone(&output))),
        );
        let event = ObservedEvent::metric("queue_depth")
            .context(
                ObserveContext::default()
                    .with_service("worker")
                    .with_route("/jobs"),
            )
            .field("active", true, FieldPrivacy::Public)
            .field("depth", 42_u64, FieldPrivacy::Public)
            .field("route", "/jobs", FieldPrivacy::Public);

        tracing::subscriber::with_default(subscriber, || emit_tracing(&event));

        let output = String::from_utf8(output.lock().expect("json output lock").clone())
            .expect("json output is utf8");
        let line = output.lines().next().expect("one json line");
        let json: serde_json::Value = serde_json::from_str(line).expect("valid json log line");
        let fields = json
            .get("fields")
            .and_then(serde_json::Value::as_object)
            .expect("json event fields");

        assert_eq!(fields.get("event_name").unwrap(), "queue_depth");
        assert_eq!(fields.get("event_class").unwrap(), "metric");
        assert_eq!(fields.get("observed_context_has_service").unwrap(), true);
        assert_eq!(fields.get("observed_context_service").unwrap(), "worker");
        assert_eq!(fields.get("observed_context_route").unwrap(), "/jobs");
        assert_eq!(fields.get("observed_field_count").unwrap(), 3);
        assert_eq!(fields.get("observed_field_0_name").unwrap(), "active");
        assert_eq!(fields.get("observed_field_0_kind").unwrap(), "bool");
        assert_eq!(fields.get("observed_field_0_value_bool").unwrap(), true);
        assert_eq!(fields.get("observed_field_1_name").unwrap(), "depth");
        assert_eq!(fields.get("observed_field_1_kind").unwrap(), "u64");
        assert_eq!(fields.get("observed_field_1_value_u64").unwrap(), 42);
        assert_eq!(fields.get("observed_field_2_name").unwrap(), "route");
        assert_eq!(fields.get("observed_field_2_kind").unwrap(), "string");
        assert_eq!(
            fields.get("observed_field_2_value_string").unwrap(),
            "/jobs"
        );
        assert!(!fields.contains_key("context"));
        assert!(!fields.contains_key("fields"));
    }

    #[test]
    fn emit_tracing_records_otlp_facing_typed_slots() {
        let capture = CaptureLayer::default();
        let events = Arc::clone(&capture.events);
        let subscriber = tracing_subscriber::registry().with(capture);
        let event = ObservedEvent::analytics("cta")
            .field("clicks", 7_u64, FieldPrivacy::Public)
            .field("ok", true, FieldPrivacy::Public)
            .field("ratio", 0.5_f64, FieldPrivacy::Public);

        tracing::subscriber::with_default(subscriber, || emit_tracing(&event));

        let events = events.lock().expect("captured events lock");
        let event = events.first().expect("captured event");
        assert_eq!(event.target, ANALYTICS_TARGET);
        assert!(!event.fields.contains_key("context"));
        assert!(!event.fields.contains_key("fields"));
        assert_eq!(
            event.fields.get("observed_field_0_name"),
            Some(&CapturedValue::Str("clicks".to_string()))
        );
        assert_eq!(
            event.fields.get("observed_field_0_value_u64"),
            Some(&CapturedValue::U64(7))
        );
        assert_eq!(
            event.fields.get("observed_field_1_name"),
            Some(&CapturedValue::Str("ok".to_string()))
        );
        assert_eq!(
            event.fields.get("observed_field_1_value_bool"),
            Some(&CapturedValue::Bool(true))
        );
        assert_eq!(
            event.fields.get("observed_field_2_name"),
            Some(&CapturedValue::Str("ratio".to_string()))
        );
        assert_eq!(
            event.fields.get("observed_field_2_value_f64"),
            Some(&CapturedValue::F64(0.5))
        );
    }
}
