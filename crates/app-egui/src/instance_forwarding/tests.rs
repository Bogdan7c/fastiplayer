use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use desktop_integration::{
    ForwardedInstanceAction, ForwardedInstanceRequest, ForwardedRequestAcknowledgement,
    ForwardedRequestDelivery, ForwardedRequestSink, ForwardedRequestSinkError, FullscreenIntent,
    InstanceForwardingError, InstanceForwardingServiceError, WindowActivationToken,
};

use super::execution::{ForwardedRequestTarget, execute_forwarded_request};
use super::{
    DELIVERY_MAILBOX_CAPACITY, InstanceForwardingInbox, PENDING_REQUEST_LIMIT,
    external_open_request_for,
};
use crate::app_instance::{
    ForwardingEnvironment, RunningInstanceForwarder, forward_arguments_to_running_instance,
};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::external_open::DropTarget;
use crate::external_open::dispatch_external_open_request;
use crate::external_open::pipeline_tests::RecordingHost;
use crate::external_open::request::{ExternalOpenItem, ExternalOpenRequest};
use crate::window_fullscreen::test_support::FakeFullscreenWindow;
use crate::window_fullscreen::{FullscreenWindowPort, enter_fullscreen};

/// Ящик без настоящей службы: приёмник, который получила бы служба, сохраняется
/// для теста, сама «служба» не запускается (как при отсутствии шины).
fn inbox_with_captured_sink() -> (InstanceForwardingInbox, Arc<dyn ForwardedRequestSink>) {
    let captured: Mutex<Option<Arc<dyn ForwardedRequestSink>>> = Mutex::new(None);
    let inbox = InstanceForwardingInbox::start_with(
        AppWakePort::disconnected(AppWakeOwner::InstanceForwarding),
        |_config, sink| {
            *captured.lock().expect("lock") = Some(sink);
            Err(InstanceForwardingServiceError::UnsupportedPlatform)
        },
    );
    let sink = captured
        .lock()
        .expect("lock")
        .take()
        .expect("service got a receiver");
    (inbox, sink)
}

/// Доставка, считающая свои подтверждения.
fn counted_delivery(
    request: ForwardedInstanceRequest,
    acknowledged: &Arc<AtomicUsize>,
) -> ForwardedRequestDelivery {
    let acknowledged = acknowledged.clone();
    ForwardedRequestDelivery::new(
        request,
        ForwardedRequestAcknowledgement::from_callback(move || {
            acknowledged.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

fn open_request(uris: &[&str]) -> ForwardedInstanceRequest {
    ForwardedInstanceRequest::open(uris.iter().map(|uri| (*uri).to_owned()).collect(), None)
        .expect("correct request")
}

#[test]
fn delivery_is_acknowledged_only_when_ui_drains_and_runs_in_arrival_order() {
    let (mut inbox, sink) = inbox_with_captured_sink();
    let acknowledged = Arc::new(AtomicUsize::new(0));

    sink.deliver(counted_delivery(
        open_request(&["file:///a.mkv"]),
        &acknowledged,
    ))
    .expect("drawer");
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("drawer");
    assert_eq!(
        acknowledged.load(Ordering::SeqCst),
        0,
        "Before the UI stream is parsed, the sender does not need to receive confirmation."
    );

    assert_eq!(inbox.drain_deliveries(), 2);
    assert_eq!(acknowledged.load(Ordering::SeqCst), 2);
    assert_eq!(
        inbox.take_next_pending_request(),
        Some(open_request(&["file:///a.mkv"]))
    );
    assert_eq!(
        inbox.take_next_pending_request(),
        Some(ForwardedInstanceRequest::activate(None))
    );
    assert_eq!(inbox.take_next_pending_request(), None);
}

#[test]
fn full_mailbox_is_refused_as_full_without_losing_accepted_deliveries() {
    let (mut inbox, sink) = inbox_with_captured_sink();
    let acknowledged = Arc::new(AtomicUsize::new(0));
    for _ in 0..DELIVERY_MAILBOX_CAPACITY {
        sink.deliver(counted_delivery(
            ForwardedInstanceRequest::activate(None),
            &acknowledged,
        ))
        .expect("within the container");
    }

    let overflow = sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ));

    assert_eq!(overflow, Err(ForwardedRequestSinkError::Full));
    assert_eq!(inbox.drain_deliveries(), DELIVERY_MAILBOX_CAPACITY);
    assert_eq!(
        acknowledged.load(Ordering::SeqCst),
        DELIVERY_MAILBOX_CAPACITY
    );
}

#[test]
fn shutdown_refuses_new_deliveries_and_never_acknowledges_undrained_ones() {
    let (mut inbox, sink) = inbox_with_captured_sink();
    let acknowledged = Arc::new(AtomicUsize::new(0));
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("drawer");
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("drawer");
    assert_eq!(inbox.drain_deliveries(), 2);
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("drawer");

    inbox.shutdown();
    inbox.shutdown();

    assert_eq!(
        sink.deliver(counted_delivery(
            ForwardedInstanceRequest::activate(None),
            &acknowledged
        )),
        Err(ForwardedRequestSinkError::Closed)
    );
    assert_eq!(
        acknowledged.load(Ordering::SeqCst),
        2,
        "undisassembled delivery destroyed without confirmation"
    );
    assert_eq!(inbox.drain_deliveries(), 0);
    assert_eq!(inbox.take_next_pending_request(), None, "line cleared");
}

#[test]
fn pending_requests_waiting_for_window_keep_only_the_newest() {
    let (mut inbox, sink) = inbox_with_captured_sink();
    let acknowledged = Arc::new(AtomicUsize::new(0));
    let total = PENDING_REQUEST_LIMIT + 2;
    for index in 0..total {
        sink.deliver(counted_delivery(
            open_request(&[&format!("file:///{index}.mkv")]),
            &acknowledged,
        ))
        .expect("drawer");
        // Окна ещё нет: UI-поток разбирает ящик, но ничего не исполняет.
        inbox.drain_deliveries();
    }

    let mut kept = Vec::new();
    while let Some(request) = inbox.take_next_pending_request() {
        kept.push(request);
    }
    assert_eq!(kept.len(), PENDING_REQUEST_LIMIT);
    assert_eq!(
        kept[0],
        open_request(&["file:///2.mkv"]),
        "most ancient discarded"
    );
    assert_eq!(
        kept.last(),
        Some(&open_request(&[&format!("file:///{}.mkv", total - 1)]))
    );
    assert_eq!(
        acknowledged.load(Ordering::SeqCst),
        total,
        "Every sender got a response."
    );
}

#[cfg(unix)]
#[test]
fn forwarded_uris_become_a_video_drop_with_exact_paths() {
    use std::os::unix::ffi::OsStringExt;

    assert_eq!(
        external_open_request_for(ForwardedInstanceAction::Activate),
        None
    );

    let request = open_request(&["file:///tmp/%FFclip.mkv", "https://example.org/v"]);
    let action = request.into_parts().action;
    let open = external_open_request_for(action).expect("There's something to open.");

    assert_eq!(open.target, DropTarget::Video);
    assert_eq!(
        open.items[0],
        ExternalOpenItem::LocalPath(PathBuf::from(OsString::from_vec(
            b"/tmp/\xFFclip.mkv".to_vec()
        )))
    );
    assert!(
        matches!(&open.items[1], ExternalOpenItem::WebUrl(url) if url.as_str() == "https://example.org/v")
    );
}

/// Транспорт «в том же процессе»: доставляет в настоящий ящик первого экземпляра,
/// UI-поток которого сразу разбирает ящик (подтверждение — как в жизни).
struct InProcessTransport<'inbox> {
    sink: Arc<dyn ForwardedRequestSink>,
    receiver: &'inbox RefCell<InstanceForwardingInbox>,
}

impl RunningInstanceForwarder for InProcessTransport<'_> {
    fn forward(&self, request: &ForwardedInstanceRequest) -> Result<(), InstanceForwardingError> {
        let acknowledged = Arc::new(AtomicUsize::new(0));
        self.sink
            .deliver(counted_delivery(request.clone(), &acknowledged))
            .map_err(|_| InstanceForwardingError::InstanceNotResponding)?;
        self.receiver.borrow_mut().drain_deliveries();
        if acknowledged.load(Ordering::SeqCst) == 1 {
            Ok(())
        } else {
            Err(InstanceForwardingError::InstanceNotResponding)
        }
    }
}

/// Первый экземпляр без winit: настоящий диспетчер внешнего открытия (`RecordingHost`
/// записывает решения) и настоящий вход в фуллскрин на фальшивом окне.
struct FirstInstanceUnderTest {
    host: RecordingHost,
    window: FakeFullscreenWindow,
    /// Шаги исполнения в порядке вызова.
    steps: Vec<String>,
}

impl ForwardedRequestTarget for FirstInstanceUnderTest {
    fn raise_window(&mut self, _activation_token: Option<WindowActivationToken>) {
        self.steps.push("raise".to_owned());
    }

    fn open_forwarded_media(&mut self, request: ExternalOpenRequest) {
        dispatch_external_open_request(&mut self.host, request);
        self.steps.push("open".to_owned());
    }

    fn enter_fullscreen(&mut self) {
        let entry = enter_fullscreen(&self.window);
        self.steps.push(format!("fullscreen:{entry:?}"));
    }
}

/// Что увидел первый экземпляр после исполнения пересланного запроса.
struct FirstInstanceObservation {
    /// Решения диспетчера открытия (`open:…`, `replace:…`).
    opened: Vec<String>,
    steps: Vec<String>,
    window_fullscreen: bool,
}

/// Прогоняет второй запуск с аргументами через пересылку и исполняет у первого тем же
/// исполнителем, что shell (`execute_forwarded_request`).
fn second_launch_reaches_first(
    arguments: &[OsString],
    fullscreen: FullscreenIntent,
    working_directory: &std::path::Path,
    first_window: FakeFullscreenWindow,
) -> FirstInstanceObservation {
    let (inbox, sink) = inbox_with_captured_sink();
    let inbox = RefCell::new(inbox);
    let transport = InProcessTransport {
        sink,
        receiver: &inbox,
    };

    forward_arguments_to_running_instance(
        arguments,
        fullscreen,
        ForwardingEnvironment {
            current_directory: Some(working_directory.to_path_buf()),
            activation_token: None,
        },
        &transport,
    )
    .expect("The second launch ends with code 0.");

    let request = inbox
        .borrow_mut()
        .take_next_pending_request()
        .expect("The first copy received the request");
    let mut first = FirstInstanceUnderTest {
        host: RecordingHost::default(),
        window: first_window,
        steps: Vec::new(),
    };
    execute_forwarded_request(request, &mut first);
    FirstInstanceObservation {
        opened: first.host.calls,
        steps: first.steps,
        window_fullscreen: first.window.is_fullscreen(),
    }
}

#[test]
fn second_launch_with_one_file_opens_it_in_first_like_the_open_button() {
    let folder = tempfile::tempdir().expect("time-folder");
    let file = folder.path().join("фильм 1.mkv");
    std::fs::write(&file, b"media").expect("file");

    // Второй процесс запущен в этой папке с относительным путём.
    let first = second_launch_reaches_first(
        &[OsString::from("фильм 1.mkv")],
        FullscreenIntent::LeaveAsIs,
        folder.path(),
        FakeFullscreenWindow::default(),
    );

    assert_eq!(first.opened, [format!("open:{}", file.display())]);
    assert_eq!(
        first.steps,
        ["raise", "open"],
        "no fullscreen without the flag"
    );
    assert!(!first.window_fullscreen);
}

#[test]
fn second_launch_with_several_files_asks_first_to_replace_queue_in_given_order() {
    let folder = tempfile::tempdir().expect("time-folder");
    let second = folder.path().join("b.mkv");
    let first = folder.path().join("a.mkv");
    std::fs::write(&second, b"media").expect("file");
    std::fs::write(&first, b"media").expect("file");

    let first_instance = second_launch_reaches_first(
        &[
            second.clone().into_os_string(),
            first.clone().into_os_string(),
        ],
        FullscreenIntent::LeaveAsIs,
        std::path::Path::new("/"),
        FakeFullscreenWindow::default(),
    );

    assert_eq!(
        first_instance.opened,
        [format!("replace:{}|{}", second.display(), first.display())]
    );
}

/// `fastiplayer --fullscreen film.mkv` при открытом окне: окно поднято, файл поставлен тем
/// же путём, что кнопка Open, и только потом окно ушло в фуллскрин тем же входом, что кнопка.
#[test]
fn second_launch_with_fullscreen_opens_the_file_then_enters_fullscreen() {
    let folder = tempfile::tempdir().expect("time-folder");
    let file = folder.path().join("film.mkv");
    std::fs::write(&file, b"media").expect("file");

    let first = second_launch_reaches_first(
        &[OsString::from("film.mkv")],
        FullscreenIntent::Enter,
        folder.path(),
        FakeFullscreenWindow::default(),
    );

    assert_eq!(first.opened, [format!("open:{}", file.display())]);
    assert_eq!(first.steps, ["raise", "open", "fullscreen:Requested"]);
    assert!(first.window_fullscreen);
}

/// `fastiplayer --fullscreen` без файлов (`Activate`) тоже переводит окно в фуллскрин.
#[test]
fn second_launch_with_only_the_fullscreen_flag_enters_fullscreen() {
    let first = second_launch_reaches_first(
        &[],
        FullscreenIntent::Enter,
        std::path::Path::new("/"),
        FakeFullscreenWindow::default(),
    );

    assert!(first.opened.is_empty());
    assert_eq!(first.steps, ["raise", "fullscreen:Requested"]);
    assert!(first.window_fullscreen);
}

/// Уже полноэкранный первый экземпляр остаётся полноэкранным (флаг — не переключатель).
#[test]
fn fullscreen_flag_keeps_an_already_fullscreen_first_instance_fullscreen() {
    let first = second_launch_reaches_first(
        &[],
        FullscreenIntent::Enter,
        std::path::Path::new("/"),
        FakeFullscreenWindow::already_fullscreen(),
    );

    assert_eq!(first.steps, ["raise", "fullscreen:AlreadyFullscreen"]);
    assert!(first.window_fullscreen);
}
