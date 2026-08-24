//! Functional regressions superseded manifest network lifecycle-а.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};

use super::*;

/// Supersede обязан уложиться существенно раньше default HTTP timeout-а.
const CANCELLATION_DEADLINE: Duration = Duration::from_millis(750);

/// Loopback server удерживает выбранные response bodies и наблюдает TCP disconnect.
struct CancellableManifestServer {
    /// Exact target для production manifest boundary.
    target: HttpRequestTarget,
    /// События принятого HTTP request-а с zero-based connection index.
    started_requests: Receiver<(usize, Instant)>,
    /// События физического закрытия удерживаемого connection клиентом.
    disconnected_requests: Receiver<(usize, Instant)>,
    /// Bounded server thread; все socket waits имеют deadline.
    worker: Option<thread::JoinHandle<()>>,
}

impl CancellableManifestServer {
    /// Запускает fixture; только `responsive_request` получает завершённый body.
    fn start(expected_requests: usize, responsive_request: Option<usize>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind manifest server");
        listener
            .set_nonblocking(true)
            .expect("set manifest server nonblocking");
        let address = listener.local_addr().expect("manifest server address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/manifest"))
            .expect("manifest target");
        let (started_sender, started_requests) = mpsc::channel();
        let (disconnected_sender, disconnected_requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            serve_manifest_requests(
                listener,
                expected_requests,
                responsive_request,
                started_sender,
                disconnected_sender,
            );
        });
        Self {
            target,
            started_requests,
            disconnected_requests,
            worker: Some(worker),
        }
    }

    /// Ждёт exact следующего HTTP request-а.
    fn wait_started(&self, expected_index: usize) -> Instant {
        let (observed_index, started_at) = self
            .started_requests
            .recv_timeout(TEST_TIMEOUT)
            .expect("manifest request must start");
        assert_eq!(observed_index, expected_index);
        started_at
    }

    /// Ждёт disconnect и возвращает index для unordered rapid-supersede проверки.
    fn wait_disconnected(&self) -> (usize, Instant) {
        self.disconnected_requests
            .recv_timeout(TEST_TIMEOUT)
            .expect("superseded manifest connection must disconnect")
    }
}

impl Drop for CancellableManifestServer {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join manifest fixture");
        }
    }
}

/// Принимает bounded число connections и завершает каждый handler по deadline.
fn serve_manifest_requests(
    listener: TcpListener,
    expected_requests: usize,
    responsive_request: Option<usize>,
    started_sender: Sender<(usize, Instant)>,
    disconnected_sender: Sender<(usize, Instant)>,
) {
    let server_deadline = Instant::now() + TEST_TIMEOUT;
    let mut hanging_handlers = Vec::new();
    for request_index in 0..expected_requests {
        let mut stream = accept_until(&listener, server_deadline);
        read_request_headers(&mut stream);
        started_sender
            .send((request_index, Instant::now()))
            .expect("report started manifest request");

        if responsive_request == Some(request_index) {
            write_complete_response(&mut stream, request_index);
        } else {
            let disconnect_events = disconnected_sender.clone();
            hanging_handlers.push(thread::spawn(move || {
                hold_response_until_client_disconnect(
                    stream,
                    request_index,
                    disconnect_events,
                    server_deadline,
                );
            }));
        }
    }
    for handler in hanging_handlers {
        handler.join().expect("join hanging manifest handler");
    }
}

/// Nonblocking accept исключает вечное зависание отрицательной регрессии.
fn accept_until(listener: &TcpListener, deadline: Instant) -> TcpStream {
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                panic!("manifest request did not arrive before fixture deadline");
            }
            Err(error) => panic!("accept manifest request: {error}"),
        }
    }
}

/// Читает только request headers; fixture никогда не удерживает caller thread.
fn read_request_headers(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(CANCELLATION_DEADLINE))
        .expect("set request read timeout");
    let mut request_bytes = Vec::new();
    let mut chunk = [0_u8; 512];
    while !request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let bytes_read = stream.read(&mut chunk).expect("read manifest request");
        assert_ne!(bytes_read, 0, "manifest request ended before headers");
        request_bytes.extend_from_slice(&chunk[..bytes_read]);
    }
}

/// Публикует headers и намеренно не отправляет body до client-side abort-а.
fn hold_response_until_client_disconnect(
    mut stream: TcpStream,
    request_index: usize,
    disconnected_sender: Sender<(usize, Instant)>,
    deadline: Instant,
) {
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n")
        .expect("write hanging manifest headers");
    stream.flush().expect("flush hanging manifest headers");
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .expect("set disconnect polling timeout");

    let mut probe = [0_u8; 1];
    loop {
        match stream.read(&mut probe) {
            Ok(0) => {
                disconnected_sender
                    .send((request_index, Instant::now()))
                    .expect("report manifest disconnect");
                return;
            }
            Ok(_) => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && Instant::now() < deadline => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::NotConnected
                ) =>
            {
                disconnected_sender
                    .send((request_index, Instant::now()))
                    .expect("report reset manifest connection");
                return;
            }
            Err(error) => panic!("observe manifest disconnect: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "manifest connection remained alive until fixture deadline"
        );
    }
}

/// Возвращает маленький current body и закрывает response корректно.
fn write_complete_response(stream: &mut TcpStream, request_index: usize) {
    let body = format!("current-{request_index}");
    let response_headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response_headers.as_bytes())
        .expect("write current manifest headers");
    stream
        .write_all(body.as_bytes())
        .expect("write current manifest body");
    stream.flush().expect("flush current manifest response");
}

/// Создаёт production fetcher с shared source cancellation token-ом.
fn fetcher_for(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
) -> AdaptiveManifestFetcher {
    AdaptiveManifestFetcher::new(context(
        target,
        cancellation,
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("create manifest fetcher")
}

/// Принимает generation и выполняет nonblocking submission через public poll boundary.
fn submit_generation(
    fetcher: &mut AdaptiveManifestFetcher,
    target: &HttpRequestTarget,
    generation: u64,
) {
    fetcher
        .request(
            ManifestFetchRequest::new(target.clone(), SourceGeneration::new(generation)),
            Instant::now(),
        )
        .expect("request manifest generation");
    assert!(matches!(
        fetcher.poll(Instant::now()),
        ManifestPoll::TemporarilyUnavailable { .. }
    ));
}

/// Poll-ит до externally visible current manifest, не блокируя network worker.
fn wait_ready(fetcher: &mut AdaptiveManifestFetcher) -> ManifestResource {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match fetcher.poll(Instant::now()) {
            ManifestPoll::Ready(resource) => return resource,
            ManifestPoll::TemporarilyUnavailable { .. } => {
                assert!(Instant::now() < deadline, "manifest result timed out");
                thread::sleep(Duration::from_millis(1));
            }
            ManifestPoll::Cancelled => panic!("current manifest unexpectedly cancelled"),
            ManifestPoll::Failed(error) => panic!("current manifest failed: {error}"),
        }
    }
}

/// Supersede обязан закрыть A и начать B, не ожидая body/read timeout A.
#[test]
fn supersede_aborts_inflight_request_before_starting_current_generation() {
    let server = CancellableManifestServer::start(2, Some(1));
    let mut fetcher = fetcher_for(&server.target, CancellationToken::new());

    submit_generation(&mut fetcher, &server.target, 1);
    server.wait_started(0);
    let superseded_at = Instant::now();
    submit_generation(&mut fetcher, &server.target, 2);

    let current_started_at = server.wait_started(1);
    let (disconnected_index, disconnected_at) = server.wait_disconnected();
    assert_eq!(disconnected_index, 0);
    assert!(current_started_at.duration_since(superseded_at) < CANCELLATION_DEADLINE);
    assert!(disconnected_at.duration_since(superseded_at) < CANCELLATION_DEADLINE);

    let resource = wait_ready(&mut fetcher);
    assert_eq!(resource.generation(), SourceGeneration::new(2));
    assert_eq!(resource.bytes().as_ref(), b"current-1");
}

/// Последовательные supersede не накапливают stale публикации или hanging requests.
#[test]
fn rapid_supersedes_abort_each_previous_generation_and_publish_only_latest() {
    let server = CancellableManifestServer::start(3, Some(2));
    let mut fetcher = fetcher_for(&server.target, CancellationToken::new());

    submit_generation(&mut fetcher, &server.target, 1);
    server.wait_started(0);
    submit_generation(&mut fetcher, &server.target, 2);
    server.wait_started(1);
    submit_generation(&mut fetcher, &server.target, 3);
    server.wait_started(2);

    let disconnected = [server.wait_disconnected().0, server.wait_disconnected().0]
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(disconnected, BTreeSet::from([0, 1]));

    let resource = wait_ready(&mut fetcher);
    assert_eq!(resource.generation(), SourceGeneration::new(3));
    assert_eq!(resource.bytes().as_ref(), b"current-2");
}

/// Shared source cancellation abort-ит current future и остаётся terminal для fetcher-а.
#[test]
fn source_cancellation_aborts_current_request_without_waiting_for_timeout() {
    let server = CancellableManifestServer::start(1, None);
    let cancellation = CancellationToken::new();
    let mut fetcher = fetcher_for(&server.target, cancellation.clone());

    submit_generation(&mut fetcher, &server.target, 1);
    server.wait_started(0);
    let cancelled_at = Instant::now();
    cancellation.cancel();
    assert!(matches!(
        fetcher.poll(Instant::now()),
        ManifestPoll::Cancelled
    ));

    let (disconnected_index, disconnected_at) = server.wait_disconnected();
    assert_eq!(disconnected_index, 0);
    assert!(disconnected_at.duration_since(cancelled_at) < CANCELLATION_DEADLINE);
}

/// Drop owner-а закрывает request future без blocking join на caller thread-е.
#[test]
fn dropping_fetcher_aborts_inflight_manifest_request() {
    let server = CancellableManifestServer::start(1, None);
    let mut fetcher = fetcher_for(&server.target, CancellationToken::new());

    submit_generation(&mut fetcher, &server.target, 1);
    server.wait_started(0);
    let dropped_at = Instant::now();
    drop(fetcher);

    let (disconnected_index, disconnected_at) = server.wait_disconnected();
    assert_eq!(disconnected_index, 0);
    assert!(disconnected_at.duration_since(dropped_at) < CANCELLATION_DEADLINE);
}
