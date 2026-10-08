//! Сквозные тесты транспорта: настоящая служба и настоящий клиент на частной шине.
//!
//! Каждый тест поднимает свой `dbus-daemon` без каталогов служб, поэтому не
//! трогает рабочий стол пользователя и не может автозапустить настоящий плеер.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::DBusError;
use zbus::blocking::{Connection, connection};
use zbus::zvariant::Value;

use super::client::forward_with;
use super::service::{ForwardingServiceFailure, ServiceBackend, start_with};
use super::{
    APPLICATION_OBJECT_PATH, NOT_RESPONDING_ERROR_NAME, REJECTED_ERROR_NAME,
    SHUTTING_DOWN_ERROR_NAME,
};
use crate::instance_forwarding::{
    FASTIPLAYER_APPLICATION_ID, ForwardedInstanceAction, ForwardedInstanceRequest,
    ForwardedRequestDelivery, ForwardedRequestSink, ForwardedRequestSinkError,
    InstanceForwardingClientConfig, InstanceForwardingError, InstanceForwardingServiceConfig,
    InstanceForwardingServiceError, MAX_FORWARDED_URIS, WindowActivationToken,
};

/// Политика как у системного `session.conf`, но без `servicedir`.
const ISOLATED_BUS_CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

static NEXT_BUS_NUMBER: AtomicUsize = AtomicUsize::new(0);

struct PrivateSessionBus {
    address: String,
    child: Child,
    config_file: PathBuf,
}

impl PrivateSessionBus {
    fn spawn() -> Self {
        let config_file = std::env::temp_dir().join(format!(
            "fastiplayer-forwarding-test-{}-{}.conf",
            std::process::id(),
            NEXT_BUS_NUMBER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&config_file, ISOLATED_BUS_CONFIG).expect("bus config");
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config_file.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("hermetic dbus-daemon must start");
        let stdout = child.stdout.take().expect("dbus address pipe");
        let address = BufReader::new(stdout)
            .lines()
            .next()
            .expect("dbus address line")
            .expect("valid dbus address");
        Self {
            address,
            child,
            config_file,
        }
    }

    fn connect(&self) -> zbus::Result<Connection> {
        connection::Builder::address(self.address.as_str())?.build()
    }

    fn connect_with_timeout(&self, timeout: Duration) -> zbus::Result<Connection> {
        connection::Builder::address(self.address.as_str())?
            .method_timeout(timeout)
            .build()
    }
}

impl Drop for PrivateSessionBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config_file);
    }
}

/// Как приёмник обращается с подтверждением.
#[derive(Clone, Copy)]
enum SinkBehaviour {
    /// Сразу подтверждает (UI-поток жив).
    Acknowledge,
    /// Держит подтверждение и не отвечает (UI-поток завис).
    HoldWithoutAnswer,
    /// Уничтожает подтверждение без ответа (приложение завершается).
    DropAcknowledgement,
    /// Отказывает в приёме.
    Refuse(ForwardedRequestSinkError),
}

/// Приёмник, записывающий все доставленные запросы.
struct RecordingSink {
    behaviour: SinkBehaviour,
    received: Mutex<Vec<ForwardedInstanceRequest>>,
    held: Mutex<Vec<crate::instance_forwarding::ForwardedRequestAcknowledgement>>,
}

impl RecordingSink {
    fn new(behaviour: SinkBehaviour) -> Arc<Self> {
        Arc::new(Self {
            behaviour,
            received: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
        })
    }

    fn received(&self) -> Vec<ForwardedInstanceRequest> {
        self.received.lock().expect("sink lock").clone()
    }
}

impl ForwardedRequestSink for RecordingSink {
    fn deliver(&self, delivery: ForwardedRequestDelivery) -> Result<(), ForwardedRequestSinkError> {
        if let SinkBehaviour::Refuse(error) = self.behaviour {
            return Err(error);
        }
        let (request, acknowledgement) = delivery.into_parts();
        self.received.lock().expect("sink lock").push(request);
        match self.behaviour {
            SinkBehaviour::Acknowledge => acknowledgement.acknowledge(),
            SinkBehaviour::HoldWithoutAnswer => {
                self.held.lock().expect("sink lock").push(acknowledgement);
            }
            SinkBehaviour::DropAcknowledgement => drop(acknowledgement),
            SinkBehaviour::Refuse(_) => unreachable!("обработано выше"),
        }
        Ok(())
    }
}

const SHORT_ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_millis(300);

fn start_service(bus: &PrivateSessionBus, sink: Arc<RecordingSink>) -> ServiceBackend {
    start_with(
        || bus.connect(),
        InstanceForwardingServiceConfig {
            acknowledgement_timeout: SHORT_ACKNOWLEDGEMENT_TIMEOUT,
        },
        sink,
    )
    .expect("служба пересылки должна запуститься на частной шине")
}

fn client_config(listener_wait: Duration) -> InstanceForwardingClientConfig {
    InstanceForwardingClientConfig {
        listener_wait,
        listener_poll_interval: Duration::from_millis(20),
        call_timeout: Duration::from_secs(3),
    }
}

fn forward(
    bus: &PrivateSessionBus,
    request: &ForwardedInstanceRequest,
    listener_wait: Duration,
) -> Result<(), InstanceForwardingError> {
    let config = client_config(listener_wait);
    forward_with(
        || bus.connect_with_timeout(config.call_timeout),
        request,
        config,
    )
}

fn token(raw: &str) -> WindowActivationToken {
    WindowActivationToken::from_raw(raw.to_owned()).expect("валидный билет")
}

#[test]
fn second_instance_open_reaches_first_with_same_uris_and_activation_token() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Acknowledge);
    let _service = start_service(&bus, sink.clone());
    let uris = vec![
        "file:///home/u/%D0%B2%D0%B8%D0%B4%D0%B5%D0%BE.mkv".to_owned(),
        "file:///tmp/%FF.mkv".to_owned(),
        "https://example.org/watch?v=1".to_owned(),
    ];
    let request = ForwardedInstanceRequest::open(uris.clone(), Some(token("kwin-token-1")))
        .expect("корректный запрос");

    assert_eq!(forward(&bus, &request, Duration::from_secs(1)), Ok(()));

    let received = sink.received();
    assert_eq!(received.len(), 1);
    let ForwardedInstanceAction::Open(received_uris) = received[0].action() else {
        panic!("ожидалось Open");
    };
    let received_texts: Vec<&str> = received_uris.iter().map(|uri| uri.as_str()).collect();
    assert_eq!(received_texts, uris);
    assert_eq!(
        received[0]
            .activation_token()
            .map(WindowActivationToken::as_str),
        Some("kwin-token-1")
    );
}

#[test]
fn second_instance_without_files_only_activates_first() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Acknowledge);
    let _service = start_service(&bus, sink.clone());

    assert_eq!(
        forward(
            &bus,
            &ForwardedInstanceRequest::activate(None),
            Duration::from_secs(1)
        ),
        Ok(())
    );

    let received = sink.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].action(), &ForwardedInstanceAction::Activate);
    assert_eq!(received[0].activation_token(), None);
}

#[test]
fn hung_first_instance_gives_not_responding_and_service_keeps_working() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::HoldWithoutAnswer);
    let _service = start_service(&bus, sink.clone());
    let request = ForwardedInstanceRequest::activate(None);

    let started_at = Instant::now();
    assert_eq!(
        forward(&bus, &request, Duration::from_secs(1)),
        Err(InstanceForwardingError::InstanceNotResponding)
    );
    // Ответ пришёл по таймауту подтверждения службы, а не по таймауту вызова.
    assert!(started_at.elapsed() < Duration::from_secs(3));

    // Служба не упала: второй запрос тоже доходит до приёмника.
    assert_eq!(
        forward(&bus, &request, Duration::from_secs(1)),
        Err(InstanceForwardingError::InstanceNotResponding)
    );
    assert_eq!(sink.received().len(), 2);
}

/// Поддельный первый экземпляр, который принимает вызов и не отвечает:
/// как процесс, зависший целиком (вместе с потоком D-Bus).
struct SilentApplicationEndpoint;

#[zbus::interface(name = "org.freedesktop.Application")]
impl SilentApplicationEndpoint {
    async fn activate(&self, _platform_data: HashMap<String, zbus::zvariant::OwnedValue>) {
        async_io::Timer::after(Duration::from_secs(30)).await;
    }
}

#[test]
fn completely_frozen_first_instance_is_reported_by_call_timeout() {
    let bus = PrivateSessionBus::spawn();
    let frozen_owner = bus.connect().expect("соединение");
    frozen_owner
        .object_server()
        .at(APPLICATION_OBJECT_PATH, SilentApplicationEndpoint)
        .expect("объект");
    frozen_owner
        .request_name(FASTIPLAYER_APPLICATION_ID)
        .expect("имя свободно");

    let config = InstanceForwardingClientConfig {
        listener_wait: Duration::from_secs(1),
        listener_poll_interval: Duration::from_millis(20),
        call_timeout: Duration::from_millis(400),
    };
    let started_at = Instant::now();
    let outcome = forward_with(
        || bus.connect_with_timeout(config.call_timeout),
        &ForwardedInstanceRequest::activate(None),
        config,
    );

    assert_eq!(outcome, Err(InstanceForwardingError::InstanceNotResponding));
    assert!(started_at.elapsed() < Duration::from_secs(5));
}

#[test]
fn shutting_down_first_instance_is_reported_as_such() {
    for behaviour in [
        SinkBehaviour::DropAcknowledgement,
        SinkBehaviour::Refuse(ForwardedRequestSinkError::Closed),
    ] {
        let bus = PrivateSessionBus::spawn();
        let _service = start_service(&bus, RecordingSink::new(behaviour));
        assert_eq!(
            forward(
                &bus,
                &ForwardedInstanceRequest::activate(None),
                Duration::from_secs(1)
            ),
            Err(InstanceForwardingError::InstanceShuttingDown)
        );
    }
}

#[test]
fn full_mailbox_is_reported_as_not_responding() {
    let bus = PrivateSessionBus::spawn();
    let _service = start_service(
        &bus,
        RecordingSink::new(SinkBehaviour::Refuse(ForwardedRequestSinkError::Full)),
    );
    assert_eq!(
        forward(
            &bus,
            &ForwardedInstanceRequest::activate(None),
            Duration::from_secs(1)
        ),
        Err(InstanceForwardingError::InstanceNotResponding)
    );
}

#[test]
fn no_first_instance_gives_not_listening_after_wait() {
    let bus = PrivateSessionBus::spawn();
    let started_at = Instant::now();
    assert_eq!(
        forward(
            &bus,
            &ForwardedInstanceRequest::activate(None),
            Duration::from_millis(200)
        ),
        Err(InstanceForwardingError::InstanceNotListening)
    );
    assert!(started_at.elapsed() >= Duration::from_millis(200));
}

#[test]
fn second_instance_waits_for_first_that_is_still_starting() {
    let bus = Arc::new(PrivateSessionBus::spawn());
    let sink = RecordingSink::new(SinkBehaviour::Acknowledge);
    let late_bus = bus.clone();
    let late_sink = sink.clone();
    let late_start = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        start_service(&late_bus, late_sink)
    });

    let outcome = forward(
        &bus,
        &ForwardedInstanceRequest::activate(None),
        Duration::from_secs(5),
    );
    let _service = late_start.join().expect("поток службы");

    assert_eq!(outcome, Ok(()));
    assert_eq!(sink.received().len(), 1);
}

#[test]
fn stopped_service_releases_the_name() {
    let bus = PrivateSessionBus::spawn();
    let service = start_service(&bus, RecordingSink::new(SinkBehaviour::Acknowledge));
    service.shutdown();

    assert_eq!(
        forward(
            &bus,
            &ForwardedInstanceRequest::activate(None),
            Duration::from_millis(100)
        ),
        Err(InstanceForwardingError::InstanceNotListening)
    );
}

#[test]
fn second_service_cannot_take_the_name_of_a_running_one() {
    let bus = PrivateSessionBus::spawn();
    let _first = start_service(&bus, RecordingSink::new(SinkBehaviour::Acknowledge));

    let second = start_with(
        || bus.connect(),
        InstanceForwardingServiceConfig {
            acknowledgement_timeout: SHORT_ACKNOWLEDGEMENT_TIMEOUT,
        },
        RecordingSink::new(SinkBehaviour::Acknowledge),
    );

    assert_eq!(
        second.err(),
        Some(InstanceForwardingServiceError::BusNameUnavailable)
    );
}

/// Сырой вызов в обход клиентских проверок: так ведёт себя испорченный отправитель.
fn raw_call(
    bus: &PrivateSessionBus,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Result<()> {
    let caller = bus.connect_with_timeout(Duration::from_secs(3))?;
    caller
        .call_method(
            Some(FASTIPLAYER_APPLICATION_ID),
            APPLICATION_OBJECT_PATH,
            Some("org.freedesktop.Application"),
            method,
            body,
        )
        .map(|_reply| ())
}

fn method_error_name(error: &zbus::Error) -> Option<&str> {
    match error {
        zbus::Error::MethodError(name, _detail, _reply) => Some(name.as_str()),
        _ => None,
    }
}

#[test]
fn oversized_and_garbage_requests_are_rejected_and_first_stays_alive() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Acknowledge);
    let _service = start_service(&bus, sink.clone());
    let no_platform_data: HashMap<&str, Value<'_>> = HashMap::new();

    let too_many = vec!["file:///a"; MAX_FORWARDED_URIS + 1];
    let error = raw_call(&bus, "Open", &(too_many, &no_platform_data)).expect_err("лимит");
    assert_eq!(method_error_name(&error), Some(REJECTED_ERROR_NAME));

    let garbage = vec!["not a uri at all"];
    let error = raw_call(&bus, "Open", &(garbage, &no_platform_data)).expect_err("мусор");
    assert_eq!(method_error_name(&error), Some(REJECTED_ERROR_NAME));

    // Неверная сигнатура отвергается разбором аргументов zbus до кода службы.
    let error = raw_call(&bus, "Open", &(42_u32,)).expect_err("неверная сигнатура");
    assert!(
        method_error_name(&error).is_some(),
        "ожидался ответ-ошибка, получено {error:?}"
    );

    let error = raw_call(
        &bus,
        "ActivateAction",
        &("quit", Vec::<Value<'_>>::new(), &no_platform_data),
    )
    .expect_err("действия не поддерживаются");
    assert_eq!(method_error_name(&error), Some(REJECTED_ERROR_NAME));

    assert!(
        sink.received().is_empty(),
        "ни один плохой запрос не дошёл до приложения"
    );
    assert_eq!(
        forward(
            &bus,
            &ForwardedInstanceRequest::activate(None),
            Duration::from_secs(1)
        ),
        Ok(()),
        "после мусора служба продолжает принимать нормальные запросы"
    );
    assert_eq!(sink.received().len(), 1);
}

#[test]
fn invalid_activation_token_is_ignored_but_request_is_delivered() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Acknowledge);
    let _service = start_service(&bus, sink.clone());
    let platform_data = HashMap::from([("activation-token", Value::from("bad token\n"))]);

    raw_call(&bus, "Activate", &(&platform_data,)).expect("запрос принят");

    let received = sink.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].activation_token(), None);
}

#[test]
fn x11_desktop_startup_id_is_accepted_as_activation_token() {
    let bus = PrivateSessionBus::spawn();
    let sink = RecordingSink::new(SinkBehaviour::Acknowledge);
    let _service = start_service(&bus, sink.clone());
    let platform_data =
        HashMap::from([("desktop-startup-id", Value::from("dolphin-1234_TIME5678"))]);

    raw_call(&bus, "Activate", &(&platform_data,)).expect("запрос принят");

    assert_eq!(
        sink.received()[0]
            .activation_token()
            .map(WindowActivationToken::as_str),
        Some("dolphin-1234_TIME5678")
    );
}

#[test]
fn error_names_shared_with_client_match_service_errors() {
    let names = [
        (
            ForwardingServiceFailure::Rejected(String::new()),
            REJECTED_ERROR_NAME,
        ),
        (
            ForwardingServiceFailure::NotResponding(String::new()),
            NOT_RESPONDING_ERROR_NAME,
        ),
        (
            ForwardingServiceFailure::ShuttingDown(String::new()),
            SHUTTING_DOWN_ERROR_NAME,
        ),
    ];
    for (failure, expected_name) in names {
        assert_eq!(failure.name().as_str(), expected_name);
    }
}

#[test]
fn object_path_follows_application_id() {
    let expected = format!("/{}", FASTIPLAYER_APPLICATION_ID.replace('.', "/"));
    assert_eq!(APPLICATION_OBJECT_PATH, expected);
}
