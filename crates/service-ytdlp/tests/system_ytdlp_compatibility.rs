//! Development-only проверка совместимости системного `yt-dlp` с production boundaries Fastiplayer.

use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use fastiplayer_config::YtDlpConfig;
use service_ytdlp::{
    YtDlpTopology, extract_yt_dlp_topology_with_config, parse_yt_dlp_media_locator,
    resolve_yt_dlp_candidate_snapshot_with_config,
};
use web_media_core::{ExtractionGeneration, SourceIdentity};

/// Stable title делает локальный HTML fixture понятным в ручном `yt-dlp` diagnostic output.
const FIXTURE_TITLE: &str = "Fastiplayer yt-dlp compatibility fixture";

/// Маленький ISO BMFF prefix достаточен для безопасного HTTP probe без реального media download.
const MEDIA_PREFIX: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2";

/// Жёсткий budget не позволяет даже loopback client-у раздувать память test server-а.
const MAX_REQUEST_HEADER_BYTES: usize = 8 * 1024;

/// Локальный HTTP fixture полностью исключает зависимость проверки от внешнего сайта и сети.
struct LocalCompatibilityFixture {
    /// Exact loopback address нужен и тесту, и shutdown wake-up соединению.
    address: SocketAddr,
    /// Stop flag завершает non-blocking accept loop без убийства test process.
    stop_requested: Arc<AtomicBool>,
    /// Join handle сохраняет server errors и не позволяет молча потерять их при завершении.
    server_thread: Option<JoinHandle<io::Result<()>>>,
}

impl LocalCompatibilityFixture {
    /// Запускает bounded loopback server на свободном порту операционной системы.
    fn start() -> io::Result<Self> {
        // Port `0` просит ОС атомарно выбрать свободный loopback port без race между reserve и bind.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        // Exact назначенный address становится частью локального fixture URL.
        let address = listener.local_addr()?;
        // Non-blocking accept позволяет thread регулярно наблюдать stop flag.
        listener.set_nonblocking(true)?;
        // Shared stop flag принадлежит fixture lifecycle, а не отдельному HTTP request.
        let stop_requested = Arc::new(AtomicBool::new(false));
        // Server thread получает отдельную ссылку на тот же lifecycle flag.
        let server_stop_requested = Arc::clone(&stop_requested);
        // Thread обслуживает все запросы двух последовательных production `yt-dlp` запусков.
        let server_thread = thread::spawn(move || serve_requests(listener, &server_stop_requested));

        // Возвращаем единый owner address, stop signal и join obligation.
        Ok(Self {
            address,
            stop_requested,
            server_thread: Some(server_thread),
        })
    }

    /// Возвращает URL HTML page, которую системный `yt-dlp` извлекает generic extractor-ом.
    fn page_url(&self) -> String {
        // Loopback URL не содержит пользовательских credentials или внешних network dependencies.
        format!("http://{}/fixture", self.address)
    }

    /// Останавливает server и обязательно публикует его I/O либо panic failure вызывающему тесту.
    fn shutdown(mut self) -> Result<(), Box<dyn Error>> {
        // Release ordering гарантирует видимость stop request в server thread.
        self.stop_requested.store(true, Ordering::Release);
        // Wake-up request снимает accept/read с ожидания без sleep-based shutdown race.
        wake_fixture_server(self.address);
        // Handle извлекается exactly once, поэтому Drop больше не пытается join-ить thread.
        let server_thread = self
            .server_thread
            .take()
            .ok_or("local compatibility fixture lost its server thread")?;
        // Panic server thread является явным провалом compatibility test, а не скрытым cleanup шумом.
        let server_result = server_thread
            .join()
            .map_err(|_| "local compatibility fixture server panicked")?;
        // I/O failure также сохраняется как причина падения теста.
        server_result?;
        // Успех означает, что весь локальный fixture lifecycle завершён чисто.
        Ok(())
    }
}

impl Drop for LocalCompatibilityFixture {
    /// Аварийно завершает server, если assertion или ранний `?` оборвал normal shutdown path.
    fn drop(&mut self) {
        // Stop flag не допускает утечки фонового thread после failed assertion.
        self.stop_requested.store(true, Ordering::Release);
        // Ошибка wake-up здесь уже вторична относительно исходной ошибки теста.
        wake_fixture_server(self.address);
        // Join всё равно обязателен, иначе тест может завершиться с живым server thread.
        if let Some(server_thread) = self.server_thread.take() {
            // Drop не маскирует исходный panic вторичным panic из cleanup path.
            let _server_result = server_thread.join();
        }
    }
}

/// Обслуживает локальные HTML/media requests до явного lifecycle stop.
fn serve_requests(listener: TcpListener, stop_requested: &AtomicBool) -> io::Result<()> {
    // Loop проверяет lifecycle flag перед каждой следующей попыткой accept.
    while !stop_requested.load(Ordering::Acquire) {
        // Каждый accepted socket обслуживается синхронно: `yt-dlp` probe не требует параллелизма.
        match listener.accept() {
            // Успешный request проходит через единый HTTP response boundary.
            Ok((mut stream, _peer_address)) => serve_one_request(&mut stream)?,
            // WouldBlock означает отсутствие request, а не ошибку fixture.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // Короткая пауза не даёт development test-у крутить CPU в busy loop.
                thread::sleep(Duration::from_millis(5));
            }
            // Любая другая accept error должна провалить compatibility test.
            Err(error) => return Err(error),
        }
    }
    // Чистый выход подтверждает исполненную shutdown obligation.
    Ok(())
}

/// Разбирает только request target и отдаёт один из двух детерминированных fixture responses.
fn serve_one_request(stream: &mut TcpStream) -> io::Result<()> {
    // Read timeout ограничивает повреждённый либо оборванный локальный request.
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    // Bounded helper дочитывает request line и headers независимо от TCP packet boundaries.
    let request_bytes = read_request_headers(stream)?;
    // Lossy decoding безопасен, потому что нас интересует только ASCII request line.
    let request_text = String::from_utf8_lossy(&request_bytes);
    // Второй token стандартной request line содержит path.
    let request_path = request_text.split_whitespace().nth(1).unwrap_or("/");
    // HEAD должен получить те же headers без body.
    let is_head_request = request_text.starts_with("HEAD ");

    // Exact fixture path публикует HTML с абсолютным media URL.
    if request_path == "/fixture" {
        // Absolute URL не зависит от Host rewriting и остаётся loopback-only.
        let html_document = format!(
            "<!doctype html><html><head><title>{FIXTURE_TITLE}</title></head><body><video controls src=\"http://{}/media.mp4\"></video></body></html>",
            stream.local_addr()?
        );
        // HTML response принадлежит только generic extractor compatibility path.
        return write_response(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            html_document.as_bytes(),
            is_head_request,
        );
    }

    // Media endpoint позволяет generic extractor выполнить optional HEAD/content probe.
    if request_path == "/media.mp4" {
        // Маленький prefix никогда не скачивается production `--simulate`, но делает endpoint корректным.
        return write_response(stream, "200 OK", "video/mp4", MEDIA_PREFIX, is_head_request);
    }

    // Неизвестный request не игнорируется и получает явный bounded HTTP failure.
    write_response(
        stream,
        "404 Not Found",
        "text/plain; charset=utf-8",
        b"not found",
        is_head_request,
    )
}

/// Дочитывает HTTP headers до separator-а, EOF либо жёсткого byte budget.
fn read_request_headers(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    // Vector capacity заранее ограничена максимально допустимым размером headers.
    let mut request_bytes = Vec::with_capacity(MAX_REQUEST_HEADER_BYTES);
    // Маленький chunk поддерживает packet fragmentation без большого stack buffer.
    let mut read_chunk = [0_u8; 1024];

    // Loop завершается только на полном header separator-е либо закрытии socket-а.
    loop {
        // Остаток budget вычисляется до чтения и никогда не переполняется.
        let remaining_budget = MAX_REQUEST_HEADER_BYTES.saturating_sub(request_bytes.len());
        // Исчерпанный budget является явной InvalidData error fixture boundary.
        if remaining_budget == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local compatibility fixture request headers exceeded the byte budget",
            ));
        }
        // Read length вычисляется до mutable slice borrow и не превышает remaining budget.
        let readable_bytes = remaining_budget.min(read_chunk.len());
        // Slice не позволяет одному read превысить вычисленный bound.
        let readable_chunk = &mut read_chunk[..readable_bytes];
        // Zero bytes означают корректный EOF от local client.
        let bytes_read = stream.read(readable_chunk)?;
        // EOF завершает parsing даже для минимального shutdown request-а.
        if bytes_read == 0 {
            break;
        }
        // Exact read bytes добавляются в bounded request buffer.
        request_bytes.extend_from_slice(&readable_chunk[..bytes_read]);
        // Standard HTTP separator доказывает получение полной request metadata.
        if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    // Пустой request не имеет route и должен получить обычный 404 response.
    Ok(request_bytes)
}

/// Будит non-blocking fixture server и не превращает cleanup race во вторичную ошибку теста.
fn wake_fixture_server(address: SocketAddr) {
    // Уже завершившийся server закономерно может отвергнуть соединение.
    if let Ok(mut stream) = TcpStream::connect(address) {
        // Полный request не оставляет server thread ждать read timeout при accept race.
        let _write_result = stream.write_all(
            b"GET /fixture-shutdown HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        );
    }
}

/// Формирует минимальный HTTP/1.1 response с точным Content-Length и закрытием соединения.
fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    omit_body: bool,
) -> io::Result<()> {
    // Headers явно запрещают keep-alive, чтобы fixture не владел connection pool lifecycle.
    let response_headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // Полная запись headers обязательна до optional response body.
    stream.write_all(response_headers.as_bytes())?;
    // HEAD по HTTP contract получает Content-Length, но не получает body bytes.
    if !omit_body {
        // GET получает exact immutable fixture bytes.
        stream.write_all(body)?;
    }
    // Flush превращает успешный write в наблюдаемый process-boundary response.
    stream.flush()
}

/// Проверяет реальный system executable через candidate и topology production APIs.
#[test]
#[ignore = "development-only: requires a system yt-dlp executable; run scripts/ytdlp-compatibility.sh"]
fn system_ytdlp_reaches_candidate_and_topology_boundaries() -> Result<(), Box<dyn Error>> {
    // Один server обслуживает оба независимых production process invocation.
    let fixture = LocalCompatibilityFixture::start()?;
    // Public locator parser формирует тот же secret-safe input type, что и приложение.
    let locator = parse_yt_dlp_media_locator(&fixture.page_url())?;
    // Current default config сохраняет production API; shell runner изолирует executable внешним shim-ом.
    let yt_dlp_config = YtDlpConfig::default();

    // Closure позволяет сначала выполнить обязательный server shutdown, даже если boundary вернул ошибку.
    let compatibility_result = (|| -> Result<(), Box<dyn Error>> {
        // Candidate call запускает настоящий executable и production single-JSON parser/normalizer.
        let candidate_snapshot = resolve_yt_dlp_candidate_snapshot_with_config(
            &locator,
            SourceIdentity::new(6),
            ExtractionGeneration::new(6),
            &yt_dlp_config,
        )?;
        // Хотя бы один accepted result доказывает usable player candidate, а не только JSON syntax.
        if candidate_snapshot.accepted_candidates().next().is_none() {
            return Err("system yt-dlp returned no accepted playback candidate".into());
        }
        // Topology call запускает отдельный настоящий process с production lazy topology argv.
        let topology = extract_yt_dlp_topology_with_config(&locator, &yt_dlp_config, || false)?;
        // Single fixture обязан материализоваться как playable video, а не collection/delegation.
        if !matches!(topology, YtDlpTopology::Video(_)) {
            return Err("system yt-dlp returned a non-video topology for the fixture".into());
        }

        // Успех означает совместимость executable с обоими публичными service boundaries.
        Ok(())
    })();

    // Server lifecycle проверяется независимо от результата production boundary.
    let fixture_shutdown_result = fixture.shutdown();
    // Сначала возвращаем semantic incompatibility, если она была обнаружена.
    compatibility_result?;
    // Затем не позволяем скрыть ошибку самого deterministic fixture server.
    fixture_shutdown_result?;
    // Итоговый marker виден runner-у с `--nocapture` и облегчает ручную диагностику.
    eprintln!(
        "COMPATIBLE: candidate and topology production boundaries accepted the system yt-dlp output"
    );
    // Test success является единственным основанием для `PASSED` в shell runner-е.
    Ok(())
}
