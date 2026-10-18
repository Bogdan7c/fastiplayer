//! Loopback HTTP-origin для тестов: статусы, заголовки, Range и счётчики запросов.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// Что отвечать на путь.
#[derive(Clone)]
pub(crate) struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Побочный эффект на каждый запрос (например, отменить токен посреди загрузки).
    on_hit: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Reply {
    pub(crate) fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::status(200).with_body(body)
    }

    pub(crate) fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            on_hit: None,
        }
    }

    pub(crate) fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub(crate) fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub(crate) fn on_hit(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_hit = Some(Arc::new(hook));
        self
    }
}

/// Общие данные между потоком сервера и тестом.
#[derive(Default)]
struct OriginState {
    routes: HashMap<String, Reply>,
    hits: HashMap<String, usize>,
}

pub(crate) struct LoopbackOrigin {
    address: SocketAddr,
    state: Arc<Mutex<OriginState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LoopbackOrigin {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback origin");
        let address = listener.local_addr().expect("origin address");
        let state = Arc::new(Mutex::new(OriginState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_state, worker_stop) = (Arc::clone(&state), Arc::clone(&stop));
        let worker = thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                serve_connection(stream, &worker_state);
            }
        });
        Self {
            address,
            state,
            stop,
            worker: Some(worker),
        }
    }

    /// Базовый адрес вида `http://127.0.0.1:port`.
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(crate) fn route(&self, path: &str, reply: Reply) {
        self.state
            .lock()
            .expect("origin state")
            .routes
            .insert(path.to_owned(), reply);
    }

    /// Сколько запросов пришло на путь (query не учитывается).
    pub(crate) fn hits(&self, path: &str) -> usize {
        self.state
            .lock()
            .expect("origin state")
            .hits
            .get(path)
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for LoopbackOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Будим blocking accept, чтобы поток увидел stop.
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_connection(mut stream: TcpStream, state: &Mutex<OriginState>) {
    let request = read_request_head(&mut stream);
    let mut lines = request.lines();
    let target = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let path = target.split('?').next().unwrap_or("/").to_owned();
    let range = lines.find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("range: bytes=")
            .map(str::to_owned)
    });
    let reply = {
        let mut guard = state.lock().expect("origin state");
        *guard.hits.entry(path.clone()).or_default() += 1;
        guard.routes.get(&path).cloned()
    };
    let reply = reply.unwrap_or_else(|| Reply::status(404));
    if let Some(hook) = &reply.on_hit {
        hook();
    }
    let _ = stream.write_all(&render_response(&reply, range.as_deref()));
}

fn render_response(reply: &Reply, range: Option<&str>) -> Vec<u8> {
    let mut status = reply.status;
    let mut body = reply.body.as_slice();
    let mut extra_headers = reply.headers.clone();
    // Поддержка Range нужна progressive-демуксеру в сквозном тесте prepare.
    if let (200, Some(range)) = (status, range)
        && let Some((start, end)) = range.split_once('-')
        && let Ok(start) = start.parse::<usize>()
    {
        let total = body.len();
        let end = end
            .parse::<usize>()
            .map_or(total - 1, |end| end.min(total - 1));
        if start <= end {
            body = &body[start..=end];
            status = 206;
            extra_headers.push((
                "Content-Range".into(),
                format!("bytes {start}-{end}/{total}"),
            ));
        }
    }
    let mut head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\nAccept-Ranges: bytes\r\n",
        body.len()
    );
    for (name, value) in extra_headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut response = head.into_bytes();
    response.extend_from_slice(body);
    response
}

fn read_request_head(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}
