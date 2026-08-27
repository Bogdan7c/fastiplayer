//! Thread-local tracing capture для functional acceptance tests.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Shared structured events, записанные subscriber-ом текущего test thread-а.
#[derive(Clone, Default)]
pub(super) struct CapturedTracing {
    events: Arc<Mutex<Vec<String>>>,
}

impl CapturedTracing {
    /// Возвращает детерминированное текстовое представление captured fields.
    pub(super) fn contents(&self) -> String {
        self.events
            .lock()
            .expect("tracing capture mutex не должен быть poisoned")
            .join("\n")
    }
}

/// Visitor одного tracing event-а без зависимости теста от fmt output layout.
#[derive(Default)]
struct CapturedEventFields {
    fields: Vec<String>,
}

impl CapturedEventFields {
    /// Добавляет field в стабильной `name=value` форме.
    fn push_display(&mut self, field: &Field, value: impl fmt::Display) {
        self.fields.push(format!("{}={value}", field.name()));
    }
}

impl Visit for CapturedEventFields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push_display(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_display(field, value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_display(field, value);
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.push_display(field, value);
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.push_display(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_display(field, value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_display(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields.push(format!("{}={value:?}", field.name()));
    }
}

/// Минимальный event-only subscriber: span storage acceptance-тесту не нужен.
struct CapturingSubscriber {
    events: Arc<Mutex<Vec<String>>>,
    next_span_id: AtomicU64,
}

impl Subscriber for CapturingSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut captured_fields = CapturedEventFields::default();
        event.record(&mut captured_fields);
        let captured_event = format!(
            "level={} {}",
            event.metadata().level(),
            captured_fields.fields.join(" ")
        );
        self.events
            .lock()
            .expect("tracing capture mutex не должен быть poisoned")
            .push(captured_event);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Ставит thread-local subscriber и возвращает output вместе с lifetime guard-ом.
pub(super) fn install_tracing_capture() -> (CapturedTracing, impl Drop) {
    let captured_tracing = CapturedTracing::default();
    let subscriber = CapturingSubscriber {
        events: Arc::clone(&captured_tracing.events),
        next_span_id: AtomicU64::new(1),
    };
    let guard = tracing::subscriber::set_default(subscriber);
    (captured_tracing, guard)
}
