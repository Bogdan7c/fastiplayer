//! Thread-local tracing capture для functional acceptance tests.

use std::env;
use std::fmt;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Child-only marker одного exact tracing test-а.
///
/// Marker передаётся только через `Command::env`, поэтому параллельные parent tests
/// не меняют process-global environment друг друга.
const ISOLATED_TRACING_TEST_CHILD_ENV: &str = "RUSTIPLAYER_PLAYER_CORE_ISOLATED_TRACING_TEST_CHILD";

/// Stable stdout marker, которым child доказывает, что exact test действительно выбран.
const ISOLATED_TRACING_TEST_EXECUTION_MARKER: &str =
    "rustiplayer isolated tracing child executes exact test: ";

/// Роль текущего процесса в test-only tracing isolation protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IsolatedTracingTestProcess {
    /// Parent дождался успешного isolated child-а и не должен повторять test body.
    ParentCompleted,

    /// Exact child обязан выполнить существующий functional test body.
    ChildRunsBody,
}

/// Выполняет trace-sensitive test в отдельном однопоточном libtest process-е.
///
/// `tracing` subscriber остаётся thread-local, но callsite interest cache общий для
/// процесса. Поэтому один exact child исключает влияние параллельного seek test-а,
/// который активирует те же production `info!` callsites без capture subscriber-а.
pub(super) fn isolate_tracing_capture_test(exact_test_name: &str) -> IsolatedTracingTestProcess {
    if let Some(marked_test_name) = env::var_os(ISOLATED_TRACING_TEST_CHILD_ENV) {
        let marked_test_name = marked_test_name.to_string_lossy();
        assert_eq!(
            marked_test_name.as_ref(),
            exact_test_name,
            "isolated tracing child получил marker другого exact test-а"
        );
        println!("{ISOLATED_TRACING_TEST_EXECUTION_MARKER}{exact_test_name}");
        return IsolatedTracingTestProcess::ChildRunsBody;
    }

    let test_binary = env::current_exe().unwrap_or_else(|error| {
        panic!("не удалось определить текущий libtest executable: {error}")
    });
    let child_output = Command::new(&test_binary)
        .arg("--exact")
        .arg(exact_test_name)
        .arg("--test-threads=1")
        .arg("--nocapture")
        .env(ISOLATED_TRACING_TEST_CHILD_ENV, exact_test_name)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "не удалось запустить isolated tracing test `{exact_test_name}` через `{}`: {error}",
                test_binary.display()
            )
        });

    let child_stdout = String::from_utf8_lossy(&child_output.stdout);
    let child_stderr = String::from_utf8_lossy(&child_output.stderr);
    assert!(
        child_output.status.success(),
        "isolated tracing test `{exact_test_name}` завершился с `{}`\nstdout:\n{}\nstderr:\n{}",
        child_output.status,
        child_stdout,
        child_stderr
    );

    let expected_execution_marker =
        format!("{ISOLATED_TRACING_TEST_EXECUTION_MARKER}{exact_test_name}");
    assert!(
        child_stdout.contains(&expected_execution_marker),
        "isolated tracing child завершился успешно, но exact test `{exact_test_name}` не выполнился\nstdout:\n{child_stdout}\nstderr:\n{child_stderr}"
    );

    IsolatedTracingTestProcess::ParentCompleted
}

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
