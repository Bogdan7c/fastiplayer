use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use desktop_integration::{
    ForwardedInstanceAction, ForwardedInstanceRequest, ForwardedRequestAcknowledgement,
    ForwardedRequestDelivery, ForwardedRequestSink, ForwardedRequestSinkError,
    InstanceForwardingError, InstanceForwardingServiceError,
};

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
use crate::external_open::request::ExternalOpenItem;

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
        .expect("служба получила приёмник");
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
        .expect("корректный запрос")
}

#[test]
fn delivery_is_acknowledged_only_when_ui_drains_and_runs_in_arrival_order() {
    let (mut inbox, sink) = inbox_with_captured_sink();
    let acknowledged = Arc::new(AtomicUsize::new(0));

    sink.deliver(counted_delivery(
        open_request(&["file:///a.mkv"]),
        &acknowledged,
    ))
    .expect("ящик принял");
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("ящик принял");
    assert_eq!(
        acknowledged.load(Ordering::SeqCst),
        0,
        "до разбора UI-потоком отправитель не должен получить подтверждение"
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
        .expect("в пределах ёмкости");
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
    .expect("ящик принял");
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("ящик принял");
    assert_eq!(inbox.drain_deliveries(), 2);
    sink.deliver(counted_delivery(
        ForwardedInstanceRequest::activate(None),
        &acknowledged,
    ))
    .expect("ящик принял");

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
        "неразобранная доставка уничтожена без подтверждения"
    );
    assert_eq!(inbox.drain_deliveries(), 0);
    assert_eq!(inbox.take_next_pending_request(), None, "очередь очищена");
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
        .expect("ящик принял");
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
        "самые старые отброшены"
    );
    assert_eq!(
        kept.last(),
        Some(&open_request(&[&format!("file:///{}.mkv", total - 1)]))
    );
    assert_eq!(
        acknowledged.load(Ordering::SeqCst),
        total,
        "каждый отправитель получил ответ"
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
    let (action, _token) = request.into_parts();
    let open = external_open_request_for(action).expect("есть что открыть");

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

/// Прогоняет второй запуск с аргументами через пересылку и исполняет у первого.
fn second_launch_reaches_first(
    arguments: &[OsString],
    working_directory: &std::path::Path,
) -> Vec<String> {
    let (inbox, sink) = inbox_with_captured_sink();
    let inbox = RefCell::new(inbox);
    let transport = InProcessTransport {
        sink,
        receiver: &inbox,
    };

    forward_arguments_to_running_instance(
        arguments,
        ForwardingEnvironment {
            current_directory: Some(working_directory.to_path_buf()),
            activation_token: None,
        },
        &transport,
    )
    .expect("второй запуск завершается с кодом 0");

    let request = inbox
        .borrow_mut()
        .take_next_pending_request()
        .expect("первый экземпляр получил запрос");
    let (action, _token) = request.into_parts();
    let open = external_open_request_for(action).expect("есть что открыть");
    let mut host = RecordingHost::default();
    dispatch_external_open_request(&mut host, open);
    host.calls
}

#[test]
fn second_launch_with_one_file_opens_it_in_first_like_the_open_button() {
    let folder = tempfile::tempdir().expect("временная папка");
    let file = folder.path().join("фильм 1.mkv");
    std::fs::write(&file, b"media").expect("файл");

    // Второй процесс запущен в этой папке с относительным путём.
    let calls = second_launch_reaches_first(&[OsString::from("фильм 1.mkv")], folder.path());

    assert_eq!(calls, [format!("open:{}", file.display())]);
}

#[test]
fn second_launch_with_several_files_asks_first_to_replace_queue_in_given_order() {
    let folder = tempfile::tempdir().expect("временная папка");
    let second = folder.path().join("b.mkv");
    let first = folder.path().join("a.mkv");
    std::fs::write(&second, b"media").expect("файл");
    std::fs::write(&first, b"media").expect("файл");

    let calls = second_launch_reaches_first(
        &[
            second.clone().into_os_string(),
            first.clone().into_os_string(),
        ],
        std::path::Path::new("/"),
    );

    assert_eq!(
        calls,
        [format!("replace:{}|{}", second.display(), first.display())]
    );
}
