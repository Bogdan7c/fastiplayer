//! Hermetic concurrent HTTP origin для HDS runtime acceptance tests.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use source_core::HttpRequestTarget;

/// Read deadline не позволяет fixture connection зависнуть при malformed request-е.
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Минимальный локальный HTTP origin без внешней сети и скрытых fixture-файлов.
pub(crate) struct HermeticHttpServer {
    /// Адрес случайного loopback port-а.
    address: SocketAddr,
    /// Cooperative флаг завершения accept loop-а.
    stop: Arc<AtomicBool>,
    /// Запрошенные path-ы доказывают реальный transport traversal.
    requested_paths: Arc<Mutex<Vec<String>>>,
    /// Текущий счётчик одновременно обслуживаемых media fragment запросов.
    active_media_requests: Arc<AtomicUsize>,
    /// High-water mark доказывает фактический concurrency bound.
    maximum_concurrent_media_requests: Arc<AtomicUsize>,
    /// Каждый accepted socket получает joinable worker, чтобы fixture умел отвечать параллельно.
    connection_workers: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    /// Join handle не позволяет серверу пережить тест.
    worker: Option<thread::JoinHandle<()>>,
}

impl HermeticHttpServer {
    /// Запускает bounded origin с заранее известными immutable ответами.
    pub(crate) fn start(routes: HashMap<&'static str, Vec<u8>>) -> Self {
        Self::start_with_media_delay(routes, Duration::ZERO)
    }

    /// Запускает origin, который детерминированно задерживает только fragment responses.
    pub(crate) fn start_with_media_delay(
        routes: HashMap<&'static str, Vec<u8>>,
        media_response_delay: Duration,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HDS fixture server");
        listener
            .set_nonblocking(true)
            .expect("set HDS fixture listener nonblocking");
        let address = listener.local_addr().expect("read HDS fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let requested_paths = Arc::new(Mutex::new(Vec::new()));
        let worker_requested_paths = Arc::clone(&requested_paths);
        let active_media_requests = Arc::new(AtomicUsize::new(0));
        let worker_active_media_requests = Arc::clone(&active_media_requests);
        let maximum_concurrent_media_requests = Arc::new(AtomicUsize::new(0));
        let worker_maximum_concurrent_media_requests =
            Arc::clone(&maximum_concurrent_media_requests);
        let connection_workers = Arc::new(Mutex::new(Vec::new()));
        let worker_connection_workers = Arc::clone(&connection_workers);
        let routes = Arc::new(routes);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let connection_routes = Arc::clone(&routes);
                        let connection_requested_paths = Arc::clone(&worker_requested_paths);
                        let connection_active_media_requests =
                            Arc::clone(&worker_active_media_requests);
                        let connection_maximum_concurrent_media_requests =
                            Arc::clone(&worker_maximum_concurrent_media_requests);
                        let connection_worker = thread::spawn(move || {
                            serve_http_connection(
                                stream,
                                &connection_routes,
                                &connection_requested_paths,
                                media_response_delay,
                                &connection_active_media_requests,
                                &connection_maximum_concurrent_media_requests,
                            );
                        });
                        worker_connection_workers
                            .lock()
                            .expect("lock HDS connection workers")
                            .push(connection_worker);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("HDS fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            requested_paths,
            active_media_requests,
            maximum_concurrent_media_requests,
            connection_workers,
            worker: Some(worker),
        }
    }

    /// Возвращает exact HTTP target внутри собственного loopback origin-а.
    pub(crate) fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("valid HDS fixture target")
    }

    /// Возвращает snapshot уже обслуженных path-ов без request headers/secrets.
    pub(crate) fn requested_paths(&self) -> Vec<String> {
        self.requested_paths
            .lock()
            .expect("lock HDS requested paths")
            .clone()
    }

    /// Возвращает high-water mark одновременных fragment responses.
    pub(crate) fn maximum_concurrent_media_requests(&self) -> usize {
        self.maximum_concurrent_media_requests
            .load(Ordering::Acquire)
    }
}

impl Drop for HermeticHttpServer {
    /// Завершает accept loop и обязательно присоединяет все fixture threads.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join HDS fixture server");
        }
        for connection_worker in self
            .connection_workers
            .lock()
            .expect("lock HDS connection workers")
            .drain(..)
        {
            connection_worker
                .join()
                .expect("join HDS fixture connection worker");
        }
        assert_eq!(
            self.active_media_requests.load(Ordering::Acquire),
            0,
            "all delayed HDS fixture responses must finish before drop"
        );
    }
}

/// Обслуживает один HTTP socket и учитывает только media fragment concurrency.
fn serve_http_connection(
    mut stream: TcpStream,
    routes: &HashMap<&'static str, Vec<u8>>,
    requested_paths: &Mutex<Vec<String>>,
    media_response_delay: Duration,
    active_media_requests: &AtomicUsize,
    maximum_concurrent_media_requests: &AtomicUsize,
) {
    let request = read_http_request(&mut stream);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    requested_paths
        .lock()
        .expect("lock HDS requested paths")
        .push(path.clone());
    let is_media_fragment = path.contains("Seg") && path.contains("-Frag");
    if is_media_fragment {
        let current_media_requests = active_media_requests.fetch_add(1, Ordering::AcqRel) + 1;
        maximum_concurrent_media_requests.fetch_max(current_media_requests, Ordering::AcqRel);
        thread::sleep(media_response_delay);
    }
    let response = routes.get(path.as_str()).map_or_else(
        || http_response("404 Not Found", b"missing fixture route"),
        |body| http_response("200 OK", body),
    );
    stream
        .write_all(&response)
        .expect("write HDS fixture response");
    if is_media_fragment {
        active_media_requests.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Читает только HTTP headers; test origin не принимает request body.
fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(HTTP_READ_TIMEOUT))
        .expect("set HDS fixture read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).expect("read HDS fixture request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("HDS fixture request is UTF-8 HTTP")
}

/// Формирует закрывающий соединение HTTP/1.1 response с exact body length.
fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut response = headers.into_bytes();
    response.extend_from_slice(body);
    response
}
