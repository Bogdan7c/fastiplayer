//! Сквозные тесты входа: настоящие служба и клиент на частной шине `dbus-daemon`.
//!
//! Обычные тесты поднимают шину без каталогов служб. Тесты активации используют шину
//! с `<servicedir>`, а «запускаемый» процесс — этот же тестовый бинарь, перезапущенный
//! через скрипт-обёртку на единственный `#[ignore]`-тест-заглушку (см.
//! [`activation_stub_process`]).

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use zbus::DBusError;

use super::client::connect_with;
use super::service::{EntryServiceFailure, ServiceBackend, start_with};
use super::{NOT_RESPONDING_ERROR_NAME, REJECTED_ERROR_NAME, SHUTTING_DOWN_ERROR_NAME};
use crate::browser_session_entry::{
    BROWSER_SESSION_BUS_NAME, BROWSER_SESSION_INTERFACE, BROWSER_SESSION_OBJECT_PATH,
    BrowserEntryCallError, BrowserEntryCaller, BrowserEntryClientConfig, BrowserEntryDelivery,
    BrowserEntryServiceConfig, BrowserEntryServiceError, BrowserEntrySink, BrowserEntrySinkError,
    MAX_BROWSER_ENTRY_MESSAGE_BYTES, browser_session_activation_service_file,
};
use crate::test_support::PrivateSessionBus;

/// Как приёмник обращается со слотом ответа.
#[derive(Clone, Copy)]
enum SinkBehaviour {
    /// Отвечает `reply:<сообщение>`.
    Echo,
    /// Отвечает длиной сообщения в байтах (для сообщений у предела размера).
    ReplyWithLength,
    /// Держит слот и не отвечает (главный цикл завис).
    HoldWithoutAnswer,
    /// Уничтожает слот без ответа (приложение завершается).
    DropReply,
    /// Отвечает строкой больше лимита.
    ReplyOversize,
    /// Отказывает в приёме.
    Refuse(BrowserEntrySinkError),
}

/// Приёмник, записывающий все доставленные запросы.
struct RecordingSink {
    behaviour: SinkBehaviour,
    received: Mutex<Vec<(BrowserEntryCaller, String)>>,
    held: Mutex<Vec<crate::browser_session_entry::BrowserEntryReply>>,
}

impl RecordingSink {
    fn new(behaviour: SinkBehaviour) -> Arc<Self> {
        Arc::new(Self {
            behaviour,
            received: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
        })
    }

    fn received(&self) -> Vec<(BrowserEntryCaller, String)> {
        self.received.lock().expect("sink lock").clone()
    }
}

impl BrowserEntrySink for RecordingSink {
    fn deliver(&self, delivery: BrowserEntryDelivery) -> Result<(), BrowserEntrySinkError> {
        if let SinkBehaviour::Refuse(error) = self.behaviour {
            return Err(error);
        }
        let (request, reply) = delivery.into_parts();
        self.received
            .lock()
            .expect("sink lock")
            .push((request.caller, request.message.clone()));
        match self.behaviour {
            SinkBehaviour::Echo => reply.send(format!("reply:{}", request.message)),
            SinkBehaviour::ReplyWithLength => reply.send(request.message.len().to_string()),
            SinkBehaviour::HoldWithoutAnswer => self.held.lock().expect("sink lock").push(reply),
            SinkBehaviour::DropReply => drop(reply),
            SinkBehaviour::ReplyOversize => {
                reply.send("x".repeat(MAX_BROWSER_ENTRY_MESSAGE_BYTES + 1));
            }
            SinkBehaviour::Refuse(_) => unreachable!("handled above"),
        }
        Ok(())
    }
}

const SHORT_REPLY_TIMEOUT: Duration = Duration::from_millis(300);

fn start_service(bus: &PrivateSessionBus, sink: Arc<RecordingSink>) -> ServiceBackend {
    start_with(
        || bus.connect(),
        BrowserEntryServiceConfig {
            reply_timeout: SHORT_REPLY_TIMEOUT,
        },
        sink,
    )
    .expect("Entry service should start on a private bus")
}

fn client_config() -> BrowserEntryClientConfig {
    BrowserEntryClientConfig {
        activation_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(3),
    }
}

fn call(bus: &PrivateSessionBus, message: &str) -> Result<String, BrowserEntryCallError> {
    connect_with(|timeout| bus.connect_with_timeout(timeout), client_config())?.call(message)
}

#[test]
fn entry_round_trip_delivers_exact_message_with_caller_and_returns_exact_reply() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Echo);
    let _service = start_service(&bus, sink.clone());
    let message = r#"{"type":"hello","note":"юникод \u0000 и кавычки \""}"#;

    let reply = call(&bus, message).expect("entry must be answered");

    assert_eq!(reply, format!("reply:{message}"));
    let received = sink.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1, message);
    assert!(
        received[0].0.as_str().starts_with(':'),
        "{:?}",
        received[0].0
    );
}

#[test]
fn two_calls_on_one_client_share_one_caller_while_separate_clients_differ() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Echo);
    let _service = start_service(&bus, sink.clone());
    let client =
        connect_with(|timeout| bus.connect_with_timeout(timeout), client_config()).expect("client");

    assert_eq!(client.call("one").expect("first"), "reply:one");
    assert_eq!(client.call("two").expect("second"), "reply:two");
    let other = connect_with(|timeout| bus.connect_with_timeout(timeout), client_config())
        .expect("other client");
    assert_eq!(other.call("three").expect("third"), "reply:three");

    let received = sink.received();
    assert_eq!(received.len(), 3);
    assert_eq!(received[0].0, received[1].0, "same connection, same caller");
    assert_ne!(received[0].0, received[2].0, "new connection, new caller");
}

#[test]
fn explicit_address_bus_reaches_the_service() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Echo);
    let _service = start_service(&bus, sink);
    let client = crate::BrowserEntryClient::connect(
        crate::BrowserEntryBus::Address(bus.address.clone()),
        client_config(),
    )
    .expect("client over explicit address");
    assert_eq!(client.call("addr").expect("reply"), "reply:addr");
}

#[test]
fn caller_identifier_is_the_unique_bus_name_of_the_sender() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Echo);
    let _service = start_service(&bus, sink.clone());
    let caller_connection = bus.connect().expect("caller connection");

    let reply: String = caller_connection
        .call_method(
            Some(BROWSER_SESSION_BUS_NAME),
            BROWSER_SESSION_OBJECT_PATH,
            Some(BROWSER_SESSION_INTERFACE),
            "Entry",
            &("ping",),
        )
        .expect("direct call")
        .body()
        .deserialize()
        .expect("string reply");

    assert_eq!(reply, "reply:ping");
    let expected_caller = caller_connection.unique_name().expect("name").to_string();
    assert_eq!(sink.received()[0].0.as_str(), expected_caller);
}

#[test]
fn message_at_the_limit_passes_and_over_the_limit_is_rejected_before_delivery() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::ReplyWithLength);
    let _service = start_service(&bus, sink.clone());

    let at_limit = "a".repeat(MAX_BROWSER_ENTRY_MESSAGE_BYTES);
    assert_eq!(
        call(&bus, &at_limit).expect("limit-sized message"),
        MAX_BROWSER_ENTRY_MESSAGE_BYTES.to_string()
    );
    assert_eq!(sink.received().len(), 1);

    // Клиент отсекает лишнее локально, до обращения к шине.
    let over_limit = "a".repeat(MAX_BROWSER_ENTRY_MESSAGE_BYTES + 1);
    assert!(matches!(
        call(&bus, &over_limit),
        Err(BrowserEntryCallError::Rejected { .. })
    ));

    // Службе не доверяем клиенту: прямой вызов в обход клиента тоже отклоняется.
    let error = bus
        .connect()
        .expect("raw connection")
        .call_method(
            Some(BROWSER_SESSION_BUS_NAME),
            BROWSER_SESSION_OBJECT_PATH,
            Some(BROWSER_SESSION_INTERFACE),
            "Entry",
            &(over_limit.as_str(),),
        )
        .expect_err("service must refuse");
    match error {
        zbus::Error::MethodError(name, _detail, _reply) => {
            assert_eq!(name.as_str(), REJECTED_ERROR_NAME)
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        sink.received().len(),
        1,
        "over-limit message reached the sink"
    );
}

#[test]
fn frozen_application_gives_not_responding_within_the_reply_timeout() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::HoldWithoutAnswer);
    let _service = start_service(&bus, sink.clone());

    let started_at = Instant::now();
    let result = call(&bus, "{}");

    assert_eq!(result, Err(BrowserEntryCallError::NotResponding));
    assert!(started_at.elapsed() >= SHORT_REPLY_TIMEOUT);
    assert!(
        started_at.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started_at.elapsed()
    );
    assert_eq!(sink.received().len(), 1);
}

#[test]
fn closed_sink_gives_shutting_down_and_full_sink_gives_not_responding() {
    let bus = PrivateSessionBus::spawn();
    let closed_sink = RecordingSink::new(SinkBehaviour::Refuse(BrowserEntrySinkError::Closed));
    let service = start_service(&bus, closed_sink.clone());
    assert_eq!(call(&bus, "{}"), Err(BrowserEntryCallError::ShuttingDown));
    assert!(closed_sink.received().is_empty());
    service.shutdown();

    let full_sink = RecordingSink::new(SinkBehaviour::Refuse(BrowserEntrySinkError::Full));
    let _service = start_service(&bus, full_sink);
    assert_eq!(call(&bus, "{}"), Err(BrowserEntryCallError::NotResponding));
}

#[test]
fn dropped_reply_slot_gives_shutting_down() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::DropReply);
    let _service = start_service(&bus, sink.clone());

    assert_eq!(call(&bus, "{}"), Err(BrowserEntryCallError::ShuttingDown));
    assert_eq!(sink.received().len(), 1);
}

#[test]
fn oversized_application_reply_is_never_sent_and_gives_not_responding() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::ReplyOversize);
    let _service = start_service(&bus, sink);

    assert_eq!(call(&bus, "{}"), Err(BrowserEntryCallError::NotResponding));
}

#[test]
fn second_service_cannot_take_the_name_and_first_keeps_answering() {
    let bus = PrivateSessionBus::spawn();
    let first_sink = RecordingSink::new(SinkBehaviour::Echo);
    let _first = start_service(&bus, first_sink.clone());

    let second_sink = RecordingSink::new(SinkBehaviour::Echo);
    let second = start_with(
        || bus.connect(),
        BrowserEntryServiceConfig::default(),
        second_sink.clone(),
    );
    assert!(matches!(
        second,
        Err(BrowserEntryServiceError::BusNameUnavailable)
    ));

    assert_eq!(call(&bus, "x").expect("first answers"), "reply:x");
    assert!(second_sink.received().is_empty());
}

#[test]
fn shutdown_releases_the_name_for_a_new_service() {
    let bus = PrivateSessionBus::spawn();
    let first = start_service(&bus, RecordingSink::new(SinkBehaviour::Echo));
    first.shutdown();

    let sink = RecordingSink::new(SinkBehaviour::Echo);
    let _second = start_service(&bus, sink);
    assert_eq!(call(&bus, "again").expect("second answers"), "reply:again");
}

#[test]
fn dbus_error_names_match_the_constants_the_client_matches_on() {
    assert_eq!(
        EntryServiceFailure::Rejected(String::new()).name().as_str(),
        REJECTED_ERROR_NAME
    );
    assert_eq!(
        EntryServiceFailure::NotResponding(String::new())
            .name()
            .as_str(),
        NOT_RESPONDING_ERROR_NAME
    );
    assert_eq!(
        EntryServiceFailure::ShuttingDown(String::new())
            .name()
            .as_str(),
        SHUTTING_DOWN_ERROR_NAME
    );
}

#[test]
fn no_service_and_no_activation_file_means_not_installed() {
    let bus = PrivateSessionBus::spawn();
    assert_eq!(call(&bus, "{}"), Err(BrowserEntryCallError::NotInstalled));
}

// ---------------------------------------------------------------------------
// Активация: шина сама запускает «процесс браузерного режима».
// ---------------------------------------------------------------------------

/// Переменная окружения демона: каталог для файлов заглушки. Без неё заглушка
/// ничего не делает, поэтому при обычном `cargo test --include-ignored` она безвредна.
const STUB_DIRECTORY_VARIABLE: &str = "FASTIPLAYER_ENTRY_ACTIVATION_STUB_DIR";
/// Файл-счётчик: заглушка дописывает по строке при каждом своём старте.
const STUB_START_LOG: &str = "starts.log";
/// Сколько заглушка живёт после старта, прежде чем выйти (имитация простоя).
const STUB_LIFETIME: Duration = Duration::from_secs(4);

/// «Процесс браузерного режима»: настоящая служба входа с детерминированным ответом.
///
/// Запускается шиной через скрипт-обёртку (см. [`ActivationFixture`]); обычный
/// прогон тестов пропускает его (`#[ignore]`), а без переменной каталога он выходит.
#[test]
#[ignore = "запускается только шиной как процесс-заглушка активации"]
fn activation_stub_process() {
    let Some(stub_directory) = std::env::var_os(STUB_DIRECTORY_VARIABLE).map(PathBuf::from) else {
        return;
    };
    let bus_address = std::env::var("DBUS_STARTER_ADDRESS").expect("bus gives starter address");

    /// Приёмник заглушки: отвечает `woken:<сообщение>`.
    struct WokenSink;
    impl BrowserEntrySink for WokenSink {
        fn deliver(&self, delivery: BrowserEntryDelivery) -> Result<(), BrowserEntrySinkError> {
            let (request, reply) = delivery.into_parts();
            reply.send(format!("woken:{}", request.message));
            Ok(())
        }
    }

    let service = start_with(
        || zbus::blocking::connection::Builder::address(bus_address.as_str())?.build(),
        BrowserEntryServiceConfig::default(),
        Arc::new(WokenSink),
    )
    .expect("stub service must start");
    // Запись о старте — после того как имя занято, чтобы счётчик не считал неудачные старты.
    let mut start_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stub_directory.join(STUB_START_LOG))
        .expect("start log");
    writeln!(start_log, "started pid={}", std::process::id()).expect("write start log");
    drop(start_log);

    std::thread::sleep(STUB_LIFETIME);
    service.shutdown();
}

/// Каталоги и шина для теста активации.
struct ActivationFixture {
    root: PathBuf,
    bus: PrivateSessionBus,
}

static NEXT_FIXTURE_NUMBER: AtomicUsize = AtomicUsize::new(0);

impl ActivationFixture {
    /// `executable_directory_name` — имя каталога со скриптом; с пробелом или апострофом
    /// доказывает, что `Exec=` из генератора понимает шина.
    fn new(executable_directory_name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "fastiplayer-entry-activation-{}-{}",
            std::process::id(),
            NEXT_FIXTURE_NUMBER.fetch_add(1, Ordering::Relaxed)
        ));
        let services_directory = root.join("services");
        let executable_directory = root.join(executable_directory_name);
        std::fs::create_dir_all(&services_directory).expect("services dir");
        std::fs::create_dir_all(&executable_directory).expect("executable dir");

        let script = executable_directory.join("fastiplayer stub");
        write_stub_script(&script);
        let service_file =
            browser_session_activation_service_file(&script).expect("service file for stub");
        std::fs::write(
            services_directory.join(format!("{BROWSER_SESSION_BUS_NAME}.service")),
            service_file,
        )
        .expect("write .service");

        let stub_directory: OsString = root.clone().into_os_string();
        let bus = PrivateSessionBus::spawn_with_servicedir(
            &services_directory,
            &[(STUB_DIRECTORY_VARIABLE, stub_directory.as_os_str())],
        );
        Self { root, bus }
    }

    /// Сколько раз стартовала заглушка.
    fn stub_start_count(&self) -> usize {
        std::fs::read_to_string(self.root.join(STUB_START_LOG))
            .map(|log| log.lines().count())
            .unwrap_or(0)
    }
}

impl Drop for ActivationFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Скрипт-обёртка: перезапускает этот тестовый бинарь на тест-заглушку.
/// Аргумент `--browser-session` из `Exec=` скрипт игнорирует (libtest его не знает).
fn write_stub_script(script: &Path) {
    let test_binary = std::env::current_exe().expect("test binary path");
    let test_name = module_path!()
        .split_once("::")
        .map(|(_crate, rest)| rest)
        .expect("module path has a crate prefix");
    let contents = format!(
        "#!/bin/sh\nexec {} --exact {test_name}::activation_stub_process --ignored --test-threads=1 >/dev/null 2>&1\n",
        shell_quote(&test_binary.to_string_lossy())
    );
    std::fs::write(script, contents).expect("write stub script");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn call_through_activation(
    fixture: &ActivationFixture,
    message: &str,
) -> Result<String, BrowserEntryCallError> {
    connect_with(
        |timeout| fixture.bus.connect_with_timeout(timeout),
        client_config(),
    )?
    .call(message)
}

#[test]
fn client_wakes_a_not_running_service_through_activation_with_a_space_in_the_exec_path() {
    let fixture = ActivationFixture::new("dir with space");
    assert_eq!(fixture.stub_start_count(), 0);

    let reply = call_through_activation(&fixture, "wake").expect("activation must answer");

    assert_eq!(reply, "woken:wake");
    assert_eq!(fixture.stub_start_count(), 1);
}

#[test]
fn two_concurrent_clients_both_get_replies_from_exactly_one_started_instance() {
    let fixture = Arc::new(ActivationFixture::new("dir with space"));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|message| {
            let fixture = Arc::clone(&fixture);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                call_through_activation(&fixture, message)
            })
        })
        .collect();

    let replies: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("client thread"))
        .collect();

    assert_eq!(replies[0], Ok("woken:first".to_owned()));
    assert_eq!(replies[1], Ok("woken:second".to_owned()));
    assert_eq!(fixture.stub_start_count(), 1, "service must start once");
}

#[test]
fn activation_of_a_missing_executable_is_reported_as_activation_failed() {
    let root = std::env::temp_dir().join(format!(
        "fastiplayer-entry-missing-exec-{}",
        std::process::id()
    ));
    let services_directory = root.join("services");
    std::fs::create_dir_all(&services_directory).expect("services dir");
    let service_file = browser_session_activation_service_file(&root.join("does-not-exist"))
        .expect("service file");
    std::fs::write(
        services_directory.join(format!("{BROWSER_SESSION_BUS_NAME}.service")),
        service_file,
    )
    .expect("write .service");
    let bus = PrivateSessionBus::spawn_with_servicedir(&services_directory, &[]);

    let result = call(&bus, "{}");

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        matches!(result, Err(BrowserEntryCallError::ActivationFailed { .. })),
        "{result:?}"
    );
}

#[test]
fn frozen_bus_daemon_makes_connect_fail_with_not_responding_instead_of_hanging() {
    // Регрессия: запросы владельца и uid через `DBusProxy` игнорируют `method_timeout`.
    let bus = PrivateSessionBus::spawn();
    let config = BrowserEntryClientConfig {
        activation_timeout: Duration::from_millis(300),
        call_timeout: Duration::from_millis(300),
    };
    let started_at = Instant::now();

    let result = connect_with(
        |timeout| {
            let connection = bus.connect_with_timeout(timeout)?;
            bus.freeze();
            Ok(connection)
        },
        config,
    );

    assert!(
        matches!(result, Err(BrowserEntryCallError::NotResponding)),
        "{:?}",
        result.err()
    );
    assert!(started_at.elapsed() < Duration::from_secs(2));
}
