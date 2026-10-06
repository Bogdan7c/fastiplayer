use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context;
use demux_api::DemuxOpenError;
use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use service_ytdlp::{YtDlpRejectionReason, YtDlpServiceError};
use source_core::CancellationToken;
use web_media_transport_api::{AuthenticationFailure, TransportFailure, TransportOpenError};

use super::{WebOpenFailureReason, classify_web_open_failure};
use crate::web_media_open::ContentProbeRejection;

/// Оборачивает типизированную ошибку так же, как production: `anyhow` + слои контекста.
fn wrapped(error: impl std::error::Error + Send + Sync + 'static) -> anyhow::Error {
    Err::<(), _>(error)
        .context("Progressive transport не открыл direct mp4 (media.example.test)")
        .context("Не удалось открыть direct media URL")
        .expect_err("ошибка должна остаться ошибкой")
}

#[test]
fn transport_failures_survive_context_layers_with_exact_reason() {
    let cases = [
        (TransportFailure::NotFound, WebOpenFailureReason::NotFound),
        (TransportFailure::Gone, WebOpenFailureReason::Gone),
        (
            TransportFailure::RateLimited,
            WebOpenFailureReason::RateLimited,
        ),
        (
            TransportFailure::ServerError,
            WebOpenFailureReason::ServerError,
        ),
        (
            TransportFailure::AccessDenied,
            WebOpenFailureReason::AccessDenied,
        ),
        (
            TransportFailure::NetworkUnavailable,
            WebOpenFailureReason::NoConnection,
        ),
        (TransportFailure::Timeout, WebOpenFailureReason::TimedOut),
        (
            TransportFailure::Interrupted,
            WebOpenFailureReason::ConnectionInterrupted,
        ),
        (
            TransportFailure::InvalidResponse,
            WebOpenFailureReason::InvalidServerResponse,
        ),
    ];
    for (failure, expected) in cases {
        let error = wrapped(TransportOpenError::Transport(failure));
        assert_eq!(classify_web_open_failure(&error), expected, "{failure:?}");
    }
}

#[test]
fn expired_credentials_mean_stale_link_and_missing_mean_login() {
    let expired = wrapped(TransportOpenError::Authentication(
        AuthenticationFailure::CredentialsExpired,
    ));
    let missing = wrapped(TransportOpenError::Authentication(
        AuthenticationFailure::CredentialsMissing,
    ));

    assert_eq!(
        classify_web_open_failure(&expired),
        WebOpenFailureReason::Gone
    );
    assert_eq!(
        classify_web_open_failure(&missing),
        WebOpenFailureReason::AuthenticationRequired
    );
}

#[test]
fn yt_dlp_errors_map_to_extractor_and_site_reasons() {
    let cases = [
        (
            YtDlpServiceError::ExecutableNotFound,
            WebOpenFailureReason::ExtractorNotInstalled,
        ),
        (
            YtDlpServiceError::AdapterDisabled,
            WebOpenFailureReason::ExtractorDisabled,
        ),
        (
            YtDlpServiceError::Timeout,
            WebOpenFailureReason::ExtractorTimedOut,
        ),
        (
            YtDlpServiceError::CollectionUrl,
            WebOpenFailureReason::CollectionLink,
        ),
        (
            YtDlpServiceError::StderrLimitExceeded { limit_bytes: 10 },
            WebOpenFailureReason::ExtractorFailed,
        ),
    ];
    for (service_error, expected) in cases {
        let label = service_error.to_string();
        let error = wrapped(service_error);
        assert_eq!(classify_web_open_failure(&error), expected, "{label}");
    }

    let rejections = [
        (
            YtDlpRejectionReason::PrivateMedia,
            WebOpenFailureReason::SitePrivateMedia,
        ),
        (
            YtDlpRejectionReason::LoginRequired,
            WebOpenFailureReason::SiteLoginRequired,
        ),
        (
            YtDlpRejectionReason::GeoRestricted,
            WebOpenFailureReason::SiteGeoRestricted,
        ),
        (
            YtDlpRejectionReason::MediaUnavailable,
            WebOpenFailureReason::SiteMediaUnavailable,
        ),
        (
            YtDlpRejectionReason::UnsupportedUrl,
            WebOpenFailureReason::SiteUnsupported,
        ),
        (
            YtDlpRejectionReason::AccessDenied,
            WebOpenFailureReason::AccessDenied,
        ),
        (
            YtDlpRejectionReason::RateLimited,
            WebOpenFailureReason::RateLimited,
        ),
        (
            YtDlpRejectionReason::NetworkUnavailable,
            WebOpenFailureReason::NoConnection,
        ),
        (
            YtDlpRejectionReason::Unclassified,
            WebOpenFailureReason::SiteRejected,
        ),
    ];
    for (reason, expected) in rejections {
        let error = wrapped(YtDlpServiceError::ExtractorRejection {
            stderr_bytes: 42,
            reason,
        });
        assert_eq!(classify_web_open_failure(&error), expected, "{reason}");
    }
}

#[test]
fn demux_and_content_probe_failures_distinguish_format_from_playability() {
    assert_eq!(
        classify_web_open_failure(&wrapped(DemuxOpenError::NoMatch)),
        WebOpenFailureReason::UnrecognizedFormat
    );
    assert_eq!(
        classify_web_open_failure(&wrapped(ContentProbeRejection::NoMediaTracks)),
        WebOpenFailureReason::UnrecognizedFormat
    );
    assert_eq!(
        classify_web_open_failure(&wrapped(ContentProbeRejection::UnsupportedVideo)),
        WebOpenFailureReason::NoPlayableFormat
    );
}

/// Отмена и строковые ошибки без владельца не выдумывают причину.
#[test]
fn cancellation_and_untyped_errors_fall_back_to_unclassified() {
    let cancelled = wrapped(TransportOpenError::Cancelled);
    let untyped = anyhow::anyhow!("строковая ошибка без типизированного владельца");

    assert_eq!(
        classify_web_open_failure(&cancelled),
        WebOpenFailureReason::Unclassified
    );
    assert_eq!(
        classify_web_open_failure(&untyped),
        WebOpenFailureReason::Unclassified
    );
}

/// Внешнее звено со смыслом побеждает более глубокое.
#[test]
fn outermost_meaningful_link_wins() {
    let error = wrapped(YtDlpServiceError::ProcessFailure {
        source: anyhow::Error::new(TransportOpenError::Transport(TransportFailure::NotFound)),
    });

    assert_eq!(
        classify_web_open_failure(&error),
        WebOpenFailureReason::ExtractorFailed
    );
}

/// Обёртка без смысла (backend demux-а) пропускается, причина находится глубже.
#[test]
fn meaningless_wrapper_is_skipped_to_deeper_reason() {
    let error = wrapped(DemuxOpenError::FactoryRejected {
        factory_id: demux_api::DemuxFactoryId::new("test-factory")
            .expect("статический factory id валиден"),
        source: demux_api::DemuxFactoryOpenError::Backend(anyhow::Error::new(
            TransportOpenError::Transport(TransportFailure::ServerError),
        )),
    });

    assert_eq!(
        classify_web_open_failure(&error),
        WebOpenFailureReason::ServerError
    );
}

/// Backend demux-а отказал без сетевой причины под ним: байты пришли, но плеер их не
/// разобрал — «формат не распознан», а не «не удалось получить видео».
#[test]
fn demux_backend_failure_without_deeper_cause_is_unrecognized_format() {
    let error = wrapped(DemuxOpenError::FactoryRejected {
        factory_id: demux_api::DemuxFactoryId::new("test-factory")
            .expect("статический factory id валиден"),
        source: demux_api::DemuxFactoryOpenError::Backend(anyhow::anyhow!(
            "Неподдерживаемый формат: mp4 is not streamable"
        )),
    });

    assert_eq!(
        classify_web_open_failure(&error),
        WebOpenFailureReason::UnrecognizedFormat
    );
}

/// Как должен ответить loopback-сервер на каждое соединение.
#[derive(Clone, Copy)]
enum LoopbackResponse {
    /// Заголовок ответа без body.
    Status(&'static str),
    /// `200 OK` с телом, которое не является media.
    NonMediaBody,
    /// Закрыть соединение без ответа.
    Close,
}

/// Минимальный HTTP-сервер на 127.0.0.1 с явной остановкой (без утечки потока).
struct LoopbackServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    fn spawn(response: LoopbackResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("local address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, response),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// URL с userinfo и query: классификация не должна от них зависеть.
    fn media_url(&self) -> String {
        format!("http://user:secret@{}/clip.mp4?token=hidden", self.address)
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.join().expect("loopback server thread");
        }
    }
}

/// Читает заголовки запроса и пишет заданный ответ.
fn answer(mut stream: TcpStream, response: LoopbackResponse) {
    stream
        .set_nonblocking(false)
        .expect("blocking accepted stream");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let mut request_head = Vec::new();
    let mut byte = [0_u8; 1];
    while !request_head.ends_with(b"\r\n\r\n") && request_head.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(1) => request_head.push(byte[0]),
            _ => break,
        }
    }
    let raw_response = match response {
        LoopbackResponse::Status(status_line) => {
            format!("HTTP/1.1 {status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        }
        LoopbackResponse::NonMediaBody => {
            let body = "<html><body>это не видео</body></html>".repeat(64);
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        LoopbackResponse::Close => return,
    };
    // Клиент мог уже закрыть соединение — для теста это не ошибка сервера.
    let _ignored_write = stream.write_all(raw_response.as_bytes());
}

/// Настоящий `open_direct_media` против loopback-сервера → причина для пользователя.
fn direct_open_reason(response: LoopbackResponse) -> WebOpenFailureReason {
    let server = LoopbackServer::spawn(response);
    let locator = crate::direct_progressive_open::classify_direct_media_url(&server.media_url())
        .expect("loopback mp4 URL должен классифицироваться как direct media");
    let error = match crate::direct_progressive_open::open_direct_media(
        &locator,
        &NetworkConfig::default(),
        &PlayerDemuxConfig::default(),
        CancellationToken::new(),
    ) {
        Ok(_) => panic!("сервер без media не может дать открытый demuxer"),
        Err(error) => error,
    };
    classify_web_open_failure(&error)
}

/// Сквозной путь прямой ссылки: 404 — «не найдено», а не «нет сети»; 403, 429,
/// обрыв соединения и не-media тело дают разные причины.
#[test]
fn real_direct_open_against_loopback_server_yields_distinct_reasons() {
    assert_eq!(
        direct_open_reason(LoopbackResponse::Status("404 Not Found")),
        WebOpenFailureReason::NotFound
    );
    assert_eq!(
        direct_open_reason(LoopbackResponse::Status("403 Forbidden")),
        WebOpenFailureReason::AccessDenied
    );
    assert_eq!(
        direct_open_reason(LoopbackResponse::Status("429 Too Many Requests")),
        WebOpenFailureReason::RateLimited
    );
    assert_eq!(
        direct_open_reason(LoopbackResponse::Close),
        WebOpenFailureReason::NoConnection
    );
    assert_eq!(
        direct_open_reason(LoopbackResponse::NonMediaBody),
        WebOpenFailureReason::UnrecognizedFormat
    );
}

/// Запускает production `Command` настоящего extractor adapter-а с заданным `PATH`.
#[cfg(unix)]
struct PathOverrideLauncher {
    /// Единственный каталог поиска программ для дочернего процесса.
    search_directory: std::path::PathBuf,
}

#[cfg(unix)]
impl service_ytdlp::ExtractorProcessLauncher for PathOverrideLauncher {
    fn spawn(
        &self,
        command: &mut std::process::Command,
        _invocation: service_ytdlp::ExtractorProcessInvocation,
    ) -> std::io::Result<std::process::Child> {
        // `Command::env("PATH")` влияет и на поиск самой программы: так тест подменяет
        // PATH только дочернему процессу, не трогая окружение тестового процесса.
        command.env("PATH", &self.search_directory);
        command.spawn()
    }
}

/// Прогоняет настоящий candidate resolve через adapter и классифицирует ошибку так,
/// как её увидит `prepare_yt_dlp_web_media` (anyhow + контекст).
#[cfg(unix)]
fn yt_dlp_resolve_reason(search_directory: &std::path::Path) -> WebOpenFailureReason {
    let adapter = service_ytdlp::YtDlpExtractorAdapter::with_process_launcher(Arc::new(
        PathOverrideLauncher {
            search_directory: search_directory.to_path_buf(),
        },
    ));
    let locator = service_ytdlp::parse_yt_dlp_media_locator(
        "https://user:secret@video.example.test/watch?v=private&token=hidden",
    )
    .expect("тестовый locator валиден");
    let service_error = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            web_media_core::SourceIdentity::new(808),
            web_media_core::ExtractionGeneration::new(1),
            &fastiplayer_config::YtDlpConfig {
                resolve_timeout_ms: 5_000,
                ..fastiplayer_config::YtDlpConfig::default()
            },
            web_media_core::ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect_err("resolve без рабочего yt-dlp обязан упасть");
    let error = anyhow::Error::new(service_error)
        .context("Не удалось подготовить exact YtDlp candidate snapshot");
    classify_web_open_failure(&error)
}

/// yt-dlp нет в PATH → «не найдена программа yt-dlp», а не общий сбой процесса.
#[cfg(unix)]
#[test]
fn yt_dlp_missing_from_path_is_extractor_not_installed() {
    let empty_directory = tempfile::TempDir::new().expect("пустой каталог PATH");

    assert_eq!(
        yt_dlp_resolve_reason(empty_directory.path()),
        WebOpenFailureReason::ExtractorNotInstalled
    );
}

/// Подставной yt-dlp печатает настоящую формулировку отказа → причина доходит до
/// классификатора через процесс, stderr reader и typed ошибку сервиса.
#[cfg(unix)]
#[test]
fn yt_dlp_private_video_rejection_reaches_user_reason() {
    use std::os::unix::fs::PermissionsExt;

    let script_directory = tempfile::TempDir::new().expect("каталог подставного yt-dlp");
    let executable = script_directory.path().join("yt-dlp");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf 'ERROR: [youtube] abc: Private video. Sign in if you have access\\n' >&2\nexit 1\n",
    )
    .expect("записать подставной yt-dlp");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("сделать подставной yt-dlp исполняемым");

    assert_eq!(
        yt_dlp_resolve_reason(script_directory.path()),
        WebOpenFailureReason::SitePrivateMedia
    );
}
