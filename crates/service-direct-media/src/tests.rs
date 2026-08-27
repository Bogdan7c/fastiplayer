use std::error::Error as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use symphonia_demux::DemuxSeekability;

use super::{
    DirectMediaExtension, DirectMediaOpenError, DirectMediaUrlUnsupportedReason,
    open_direct_media_url, parse_direct_media_url,
};

/// Поведение локального HTTP server-а для direct media tests.
#[derive(Clone, Copy)]
enum TestHttpBehavior {
    /// Всегда отдавать `200 OK`, даже если клиент прислал Range.
    IgnoreRange,

    /// Отдавать `206 Partial Content` для Range-запросов.
    ServeRange,
}

/// Минимальный локальный HTTP server с Range support для unit/integration tests.
struct TestHttpServer {
    /// Адрес, на который можно собрать direct media URL.
    address: SocketAddr,

    /// Флаг остановки accept loop.
    stop: Arc<AtomicBool>,

    /// Thread с blocking accept loop.
    join_handle: Option<JoinHandle<()>>,
}

impl TestHttpServer {
    /// Запускает server на loopback без внешней сети.
    fn spawn(body: Vec<u8>, behavior: TestHttpBehavior) -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("test HTTP server должен bind-иться");
        let address = listener
            .local_addr()
            .expect("test HTTP server должен знать local_addr");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let shared_body = Arc::new(body);
        let join_handle = thread::Builder::new()
            .name("direct-media-test-http".to_string())
            .spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _peer)) = listener.accept() else {
                        break;
                    };

                    if thread_stop.load(Ordering::SeqCst) {
                        break;
                    }

                    handle_test_http_connection(&mut stream, &shared_body, behavior);
                }
            })
            .expect("test HTTP server thread должен стартовать");

        Self {
            address,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Возвращает URL с заданным path.
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Err(error) = TcpStream::connect(self.address) {
            eprintln!("test HTTP server shutdown connect failed: {error}");
        }

        if let Some(join_handle) = self.join_handle.take()
            && join_handle.join().is_err()
        {
            eprintln!("test HTTP server thread panicked during shutdown");
        }
    }
}

/// Обрабатывает один HTTP request и закрывает соединение.
fn handle_test_http_connection(stream: &mut TcpStream, body: &[u8], behavior: TestHttpBehavior) {
    let Some(request) = read_test_http_request(stream) else {
        return;
    };

    match (behavior, range_header(&request)) {
        (TestHttpBehavior::ServeRange, Some(range_header)) => {
            respond_with_range(stream, body, range_header);
        }
        _ => {
            respond_with_full_body(stream, body);
        }
    }
}

/// Читает HTTP headers до пустой строки.
fn read_test_http_request(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(300)))
        .expect("test stream timeout должен установиться");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let bytes_read = stream.read(&mut buffer).ok()?;
        if bytes_read == 0 {
            return None;
        }

        request.extend_from_slice(&buffer[..bytes_read]);

        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }

        if request.len() > 16 * 1024 {
            return None;
        }
    }

    String::from_utf8(request).ok()
}

/// Возвращает `Range` header из тестового request-а.
fn range_header(request: &str) -> Option<&str> {
    request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("range") {
            return Some(value.trim());
        }

        None
    })
}

/// Отдаёт `206 Partial Content` по byte range.
fn respond_with_range(stream: &mut TcpStream, body: &[u8], range_header: &str) {
    let Some((start, requested_end)) = parse_range_header(range_header) else {
        respond_with_status(stream, 416, "Range Not Satisfiable", &[]);
        return;
    };

    if start >= body.len() {
        respond_with_status(stream, 416, "Range Not Satisfiable", &[]);
        return;
    }

    let end = requested_end.min(body.len().saturating_sub(1));
    let response_body = &body[start..=end];
    let headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        response_body.len(),
        start,
        end,
        body.len()
    );

    stream
        .write_all(headers.as_bytes())
        .expect("test HTTP Range response headers должны записаться");
    stream
        .write_all(response_body)
        .expect("test HTTP Range response body должен записаться");
}

/// Отдаёт `200 OK` со всем body.
fn respond_with_full_body(stream: &mut TcpStream, body: &[u8]) {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );

    stream
        .write_all(headers.as_bytes())
        .expect("test HTTP full response headers должны записаться");
    stream
        .write_all(body)
        .expect("test HTTP full response body должен записаться");
}

/// Отдаёт status без body для error paths.
fn respond_with_status(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );

    stream
        .write_all(headers.as_bytes())
        .expect("test HTTP status response headers должны записаться");
    stream
        .write_all(body)
        .expect("test HTTP status response body должен записаться");
}

/// Парсит `Range: bytes=start-end`.
fn parse_range_header(range_header: &str) -> Option<(usize, usize)> {
    let byte_range = range_header.strip_prefix("bytes=")?;
    let (start, end) = byte_range.split_once('-')?;

    Some((start.parse().ok()?, end.parse().ok()?))
}

/// Возвращает минимальный структурированный PCM WAV для HTTP Range regression.
fn minimal_pcm_wav() -> Vec<u8> {
    let sample_rate_hz = 8_000_u32;
    let channel_count = 1_u16;
    let bits_per_sample = 16_u16;
    let samples = [0_i16, 1_024, -1_024, 0_i16];
    let data_length = u32::try_from(samples.len() * std::mem::size_of::<i16>())
        .expect("test WAV data length fits u32");
    let block_alignment = channel_count * (bits_per_sample / 8);
    let mut bytes = Vec::with_capacity(44 + data_length as usize);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36_u32 + data_length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channel_count.to_le_bytes());
    bytes.extend_from_slice(&sample_rate_hz.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate_hz * u32::from(block_alignment)).to_le_bytes());
    bytes.extend_from_slice(&block_alignment.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_length.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    bytes
}

/// Читает один explicit local media path для ignored manual acceptance test-а.
fn selected_manual_media_path() -> PathBuf {
    let path = std::env::var_os("RUSTIPLAYER_MEDIA_PATH")
        .map(PathBuf::from)
        .expect("RUSTIPLAYER_MEDIA_PATH must select a local file");
    assert!(
        path.is_file(),
        "selected media path is not a regular file: {}",
        path.display()
    );
    path
}

/// Выводит поддерживаемое direct-media расширение выбранного файла без угадывания имени.
fn selected_direct_extension(path: &Path) -> &str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .expect("selected direct-media path must have a UTF-8 extension");
    match extension.to_ascii_lowercase().as_str() {
        "mp4" => "mp4",
        "mov" => "mov",
        "mkv" => "mkv",
        "webm" => "webm",
        _ => panic!("selected direct-media extension is unsupported: {extension}"),
    }
}

/// Проверяет real selected media через тот же HTTP Range -> Symphonia путь, что и production.
fn assert_selected_media_opens_over_http_range(path: &Path) {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("read selected media {}: {error}", path.display()));
    let extension = selected_direct_extension(path);
    let server = TestHttpServer::spawn(bytes, TestHttpBehavior::ServeRange);
    let locator = parse_direct_media_url(&server.url(&format!("/selected.{extension}")))
        .expect("selected test URL должен пройти pure classification");
    let opened_media = open_direct_media_url(
        &locator,
        &NetworkConfig::default(),
        &PlayerDemuxConfig::default(),
    )
    .expect("selected Range-backed media must open through Symphonia");
    assert!(
        !opened_media.tracks().is_empty(),
        "selected Range-backed media must expose at least one public track"
    );
}

#[test]
fn direct_url_accepts_supported_extension_on_ip() {
    let secret = "http://user:password@127.0.0.1:9001/media/video.MP4?token=1#private";
    let parsed = parse_direct_media_url(secret)
        .expect("IP URL с supported extension должен пройти классификацию");

    assert_eq!(parsed.extension(), DirectMediaExtension::Mp4);
    assert_eq!(parsed.expose_secret_for_open(), secret);
    assert_eq!(parsed.expose_secret_for_persistence(), secret);
    assert!(parsed.requires_sensitive_persistence_acknowledgement());
    let formatted = format!("{parsed:?} {parsed}");
    assert!(!formatted.contains("password"));
    assert!(!formatted.contains("token"));
    assert!(!formatted.contains("/media/video"));
}

#[test]
fn direct_url_accepts_quicktime_mov_as_iso_bmff_extension() {
    let parsed = parse_direct_media_url("https://media.example.test/camera/ios-hevc-main10-aac-4k60.MOV")
        .expect("QuickTime .mov должен идти через direct media ISO BMFF path");

    assert_eq!(parsed.extension(), DirectMediaExtension::Mov);
    assert_eq!(parsed.extension().as_extension_hint(), "mov");
    assert!(!parsed.requires_sensitive_persistence_acknowledgement());
}

#[test]
fn direct_url_rejects_hostname_without_extension() {
    let error =
        parse_direct_media_url("https://user:password@media.example.test/private?token=secret")
            .expect_err("URL без extension должен быть rejected");

    assert!(matches!(
        &error,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::MissingExtension,
            ..
        }
    ));
    let formatted = format!("{error:?} {error}");
    assert!(!formatted.contains("password"));
    assert!(!formatted.contains("secret"));
    assert!(!formatted.contains("private"));
}

#[test]
fn invalid_syntax_error_does_not_reflect_secret_input() {
    let error = parse_direct_media_url("https://user:password@[invalid]?token=secret")
        .expect_err("invalid syntax должна возвращать typed error");
    let formatted = format!("{error:?} {error}");

    assert!(!formatted.contains("password"));
    assert!(!formatted.contains("secret"));
    assert!(matches!(error, DirectMediaOpenError::InvalidUrl { .. }));
}

/// Alternate anyhow report должен печатать каждый typed source layer ровно один раз.
#[test]
fn direct_media_error_report_does_not_duplicate_source_layers() {
    let locator = parse_direct_media_url("https://example.invalid/video.webm")
        .expect("test locator should satisfy direct-media classification");
    let direct_error = DirectMediaOpenError::TransportOpen {
        locator,
        source: web_media_transport_api::TransportOpenError::Transport(
            web_media_transport_api::TransportFailure::AccessDenied,
        ),
    };
    let source = direct_error
        .source()
        .expect("transport-open variant must preserve typed source");
    let diagnostic = format!("Не удалось открыть direct media URL: {direct_error}: {source}");

    assert_eq!(diagnostic.matches("HTTP transport не открыл").count(), 1);
    assert_eq!(diagnostic.matches("transport open failed").count(), 1);
    assert_eq!(
        diagnostic
            .matches("transport remote endpoint отказал в доступе")
            .count(),
        1
    );
}

#[test]
fn direct_url_rejects_unsupported_protocol() {
    let error = parse_direct_media_url("rtsp://media.example.test/video.mp4")
        .expect_err("RTSP остаётся future feature");

    assert!(matches!(
        error,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::UnsupportedProtocol,
            ..
        }
    ));
}

#[test]
fn direct_url_rejects_manifest_extension_as_future_feature() {
    let error = parse_direct_media_url("https://media.example.test/live/playlist.m3u8")
        .expect_err("HLS manifest не должен открываться как direct file");

    assert!(matches!(
        error,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::ManifestUnsupported,
            ..
        }
    ));
}

#[test]
fn unsupported_extension_error_does_not_reflect_path_payload() {
    let error = parse_direct_media_url(
        "https://media.example.test/private/password.token?signature=secret",
    )
    .expect_err("unsupported extension должна вернуть typed rejection");
    let formatted = format!("{error:?} {error}");

    assert!(matches!(
        error,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::UnsupportedExtension,
            ..
        }
    ));
    assert!(!formatted.contains("private"));
    assert!(!formatted.contains("password"));
    assert!(!formatted.contains("token"));
    assert!(!formatted.contains("secret"));
    assert!(formatted.len() < 512);
}

#[test]
fn non_range_http_source_opens_progressive_without_range() {
    let server = TestHttpServer::spawn(minimal_pcm_wav(), TestHttpBehavior::IgnoreRange);
    let locator = parse_direct_media_url(&server.url("/video.mp4"))
        .expect("test URL должен пройти pure classification");
    let opened = open_direct_media_url(
        &locator,
        &NetworkConfig::default(),
        &PlayerDemuxConfig::default(),
    )
    .expect("HTTP 200 probe response должен продолжиться progressive path-ом");

    assert!(matches!(
        opened.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    assert!(!opened.tracks().is_empty());
}

#[test]
fn range_backed_http_source_opens_structured_wav_through_symphonia() {
    let server = TestHttpServer::spawn(minimal_pcm_wav(), TestHttpBehavior::ServeRange);
    let locator = parse_direct_media_url(&server.url("/media.mp4"))
        .expect("test URL должен пройти pure classification");
    let opened_media = open_direct_media_url(
        &locator,
        &NetworkConfig::default(),
        &PlayerDemuxConfig::default(),
    )
    .expect("Range-backed structured WAV должен открыться через Symphonia");

    assert!(
        opened_media.tracks().iter().any(|track| {
            let codec_id = track.codec_id.to_ascii_lowercase();
            codec_id.contains("pcm")
        }),
        "direct Range source должен вернуть PCM audio track"
    );
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn selected_media_opens_over_direct_http_range() {
    let path = selected_manual_media_path();
    assert_selected_media_opens_over_http_range(&path);
}
