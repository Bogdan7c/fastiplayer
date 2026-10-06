use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use zbus::blocking::{Connection, connection};
use zbus::zvariant::OwnedValue;

use super::{
    NOTIFICATIONS_OBJECT_PATH, NOTIFICATIONS_SERVICE, escape_notification_markup, send_via,
};
use crate::notification::{CriticalDesktopNotification, DesktopNotificationError};

/// Конфиг частной шины: политика как в системном `session.conf`, но без каталогов
/// D-Bus сервисов. Иначе шина пытается автозапустить настоящую службу уведомлений
/// рабочего стола, и тест зависит от неё.
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

/// Уникальный номер шины внутри тестового процесса.
static NEXT_BUS_NUMBER: AtomicUsize = AtomicUsize::new(0);

/// Частный dbus-daemon: тест не трогает настоящий рабочий стол пользователя.
struct PrivateSessionBus {
    address: String,
    child: Child,
    config_file: PathBuf,
}

impl PrivateSessionBus {
    fn spawn() -> Self {
        let config_file = std::env::temp_dir().join(format!(
            "fastiplayer-notification-test-{}-{}.conf",
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
}

impl Drop for PrivateSessionBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config_file);
    }
}

/// Что поддельная служба получила в вызове `Notify`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReceivedNotification {
    application_name: String,
    replaces_id: u32,
    icon_name: String,
    title: String,
    body: String,
    actions: Vec<String>,
    urgency: Option<u8>,
    expire_timeout: i32,
}

/// Поддельная служба уведомлений, записывающая каждый вызов.
struct FakeNotificationService {
    received: Arc<Mutex<Vec<ReceivedNotification>>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FakeNotificationService {
    #[expect(
        clippy::too_many_arguments,
        reason = "сигнатура Notify задана спецификацией freedesktop"
    )]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|value| value.downcast_ref::<u8>().ok());
        self.received
            .lock()
            .expect("fake service lock")
            .push(ReceivedNotification {
                application_name: app_name,
                replaces_id,
                icon_name: app_icon,
                title: summary,
                body,
                actions,
                urgency,
                expire_timeout,
            });
        1
    }
}

fn serve_fake_notifications(
    bus: &PrivateSessionBus,
) -> (Connection, Arc<Mutex<Vec<ReceivedNotification>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    let service = FakeNotificationService {
        received: received.clone(),
    };
    let server = connection::Builder::address(bus.address.as_str())
        .expect("server address")
        .serve_at(NOTIFICATIONS_OBJECT_PATH, service)
        .expect("serve fake notifications")
        .name(NOTIFICATIONS_SERVICE)
        .expect("fake service name")
        .build()
        .expect("fake notification service connection");
    (server, received)
}

#[test]
fn critical_notification_reaches_service_with_escaped_body_and_no_expiry() {
    let bus = PrivateSessionBus::spawn();
    let (_server, received) = serve_fake_notifications(&bus);

    let outcome = send_via(
        || bus.connect(),
        CriticalDesktopNotification {
            title: "Fastiplayer не запустился",
            body: "Нет Vulkan <драйвера> & окна",
        },
    );

    assert_eq!(outcome, Ok(()));
    assert_eq!(
        *received.lock().expect("received lock"),
        [ReceivedNotification {
            application_name: "Fastiplayer".to_owned(),
            replaces_id: 0,
            icon_name: "dialog-error".to_owned(),
            title: "Fastiplayer не запустился".to_owned(),
            body: "Нет Vulkan &lt;драйвера&gt; &amp; окна".to_owned(),
            actions: Vec::new(),
            urgency: Some(2),
            expire_timeout: 0,
        }]
    );
}

#[test]
fn bus_without_notification_service_is_rejection_not_bus_failure() {
    let bus = PrivateSessionBus::spawn();

    let outcome = send_via(
        || bus.connect(),
        CriticalDesktopNotification {
            title: "title",
            body: "body",
        },
    );

    assert!(
        matches!(
            outcome,
            Err(DesktopNotificationError::NotificationServiceRejected(_))
        ),
        "{outcome:?}"
    );
}

#[test]
fn unreachable_bus_is_reported_as_bus_unavailable() {
    let outcome = send_via(
        || connection::Builder::address("unix:path=/nonexistent/fastiplayer-test-bus")?.build(),
        CriticalDesktopNotification {
            title: "title",
            body: "body",
        },
    );

    assert!(
        matches!(
            outcome,
            Err(DesktopNotificationError::SessionBusUnavailable(_))
        ),
        "{outcome:?}"
    );
}

#[test]
fn markup_escaping_keeps_plain_text_and_line_breaks() {
    assert_eq!(
        escape_notification_markup("a < b && c > d\nвторая строка"),
        "a &lt; b &amp;&amp; c &gt; d\nвторая строка"
    );
}
