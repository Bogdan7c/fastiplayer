use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

#[test]
fn controller_exposes_startup_error_for_app_state_creation() {
    let controller = StartupMediaController::new(None, Some("startup failure".to_string()));

    assert_eq!(
        controller.startup_error_message(),
        Some("startup failure".to_string())
    );
}

#[test]
fn pending_message_reports_existing_yt_dlp_job() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::StartupMedia);
    let (_result_publisher, result_receiver) = owner_mailbox(wake_port.clone());
    let controller = StartupMediaController {
        wake_port,
        initial_media: None,
        yt_dlp_startup_job: Some(YtDlpStartupJob {
            source_locator: service_ytdlp::parse_yt_dlp_media_locator(
                "https://www.youtube.com/watch?v=test",
            )
            .expect("test locator должен проходить service parse"),
            pending_message: "Подготовка YtDlp stream...".to_string(),
            result_receiver,
            join_handle: None,
            pending_result: None,
            cancellation_requested: Arc::new(AtomicBool::new(false)),
            source_cancellation: source_core::CancellationToken::new(),
        }),
        direct_media_startup_job: None,
        native_hls_startup_job: None,
        local_startup_job: None,
        startup_playlist_pending: false,
        orchestration: StartupMediaOrchestration::new(false),
        cli_url_target_draft: None,
        startup_config: None,
        system_capabilities: None,
        startup_error: None,
        terminal_shutdown_started: false,
        terminal_shutdown_completed: false,
    };

    assert!(controller.has_pending_startup_job());
    assert_eq!(
        controller.pending_message(),
        Some("Подготовка YtDlp stream...")
    );
    assert!(
        controller.startup_job_admission_error().is_some(),
        "single-startup-job boundary должен отвергать replacement"
    );
}

/// Собирает controller с synthetic worker-ом без network/media locator leakage.
fn controller_with_test_yt_dlp_thread(join_handle: JoinHandle<()>) -> StartupMediaController {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::StartupMedia);
    let (_result_publisher, result_receiver) = owner_mailbox(wake_port.clone());
    StartupMediaController {
        wake_port,
        initial_media: None,
        yt_dlp_startup_job: Some(YtDlpStartupJob {
            source_locator: service_ytdlp::parse_yt_dlp_media_locator(
                "https://www.youtube.com/watch?v=test",
            )
            .expect("test locator должен проходить service parse"),
            pending_message: "Подготовка YtDlp stream...".to_string(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested: Arc::new(AtomicBool::new(false)),
            source_cancellation: source_core::CancellationToken::new(),
        }),
        direct_media_startup_job: None,
        native_hls_startup_job: None,
        local_startup_job: None,
        startup_playlist_pending: false,
        orchestration: StartupMediaOrchestration::new(false),
        cli_url_target_draft: None,
        startup_config: None,
        system_capabilities: None,
        startup_error: None,
        terminal_shutdown_started: false,
        terminal_shutdown_completed: false,
    }
}

#[test]
fn startup_shutdown_timeout_retains_handle_and_later_reaps_it() {
    let release = Arc::new(AtomicBool::new(false));
    let worker_release = Arc::clone(&release);
    let mut controller = controller_with_test_yt_dlp_thread(std::thread::spawn(move || {
        while !worker_release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }));

    assert_eq!(
        controller.shutdown_until(ShutdownDeadline::after(Duration::from_millis(1))),
        ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
    );
    assert!(controller.yt_dlp_startup_job.is_some());
    assert!(
        controller
            .yt_dlp_startup_job
            .as_ref()
            .expect("timed-out job сохраняет ownership")
            .source_cancellation
            .is_cancelled(),
        "startup shutdown должен прервать transport token до bounded join"
    );

    release.store(true, Ordering::Release);
    assert_eq!(
        controller.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        ProcessOwnerShutdownOutcome::Completed
    );
    assert_eq!(
        controller.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        ProcessOwnerShutdownOutcome::AlreadyCompleted
    );
}

#[test]
fn startup_shutdown_reports_worker_panic() {
    let mut controller = controller_with_test_yt_dlp_thread(std::thread::spawn(|| {
        panic!("expected startup panic");
    }));

    assert_eq!(
        controller.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        ProcessOwnerShutdownOutcome::ThreadPanicked {
            panicked_threads: 1,
            pending_threads: 0,
        }
    );
}

#[test]
fn cli_route_keeps_local_path_unchanged() {
    let (initial_media, startup_error) = resolve_initial_media_argument(
        Some(OsString::from("/tmp/sample.mp4")),
        &AppConfig::default(),
    );

    assert!(startup_error.is_none());
    assert!(matches!(
        initial_media,
        Some(InitialMedia::File(path)) if path == Path::new("/tmp/sample.mp4")
    ));
}

#[test]
fn cli_route_classifies_each_supported_playlist_format_before_local_media_open() {
    for path in [
        "/tmp/list.m3u",
        "/tmp/list.M3U8",
        "/tmp/list.xspf",
        "/tmp/list.CUE",
    ] {
        let (initial_media, startup_error) =
            resolve_initial_media_argument(Some(path.into()), &AppConfig::default());
        assert!(startup_error.is_none(), "{path}");
        assert!(matches!(
            initial_media,
            Some(InitialMedia::Playlist(actual_path)) if actual_path == Path::new(path)
        ));
    }
}

#[cfg(unix)]
#[test]
fn cli_route_keeps_non_utf8_argument_as_native_local_path() {
    use std::os::unix::ffi::OsStringExt;

    let native_path = OsString::from_vec(b"/tmp/movie-\xFF.mkv".to_vec());
    let expected_path = std::path::PathBuf::from(native_path.clone());
    let (initial_media, startup_error) =
        resolve_initial_media_argument(Some(native_path), &AppConfig::default());

    assert!(startup_error.is_none());
    assert!(matches!(
        initial_media,
        Some(InitialMedia::File(path)) if path == expected_path
    ));
}

#[test]
fn cli_route_sends_yt_dlp_host_to_yt_dlp_path() {
    let (initial_media, startup_error) = resolve_initial_media_argument(
        Some(OsString::from("https://youtu.be/video-id")),
        &AppConfig::default(),
    );

    assert!(startup_error.is_none());
    assert!(matches!(
        initial_media,
        Some(InitialMedia::Url(locator))
            if locator.to_playlist_locator().is_ok_and(|domain_locator| {
                domain_locator.expose_secret_for_persistence()
                    == "https://youtu.be/video-id"
            })
    ));
}

#[test]
fn cli_route_sends_supported_http_media_to_direct_path() {
    let (initial_media, startup_error) = resolve_initial_media_argument(
        Some(OsString::from("https://cdn.example.test/video.mp4?token=1")),
        &AppConfig::default(),
    );

    assert!(startup_error.is_none());
    assert!(matches!(
        initial_media,
        Some(InitialMedia::Url(locator))
            if locator.to_playlist_locator().is_ok_and(|domain_locator| {
                domain_locator.expose_secret_for_persistence()
                    == "https://cdn.example.test/video.mp4?token=1"
            })
    ));
}

#[test]
fn cli_route_sends_quicktime_mov_http_media_to_direct_path() {
    let (initial_media, startup_error) = resolve_initial_media_argument(
        Some(OsString::from(
            "https://cdn.example.test/camera/ios-hevc-main10-aac-4k60.MOV",
        )),
        &AppConfig::default(),
    );

    assert!(startup_error.is_none());
    assert!(matches!(
        initial_media,
        Some(InitialMedia::Url(locator))
            if locator.to_playlist_locator().is_ok_and(|domain_locator| {
                domain_locator.expose_secret_for_persistence()
                    == "https://cdn.example.test/camera/ios-hevc-main10-aac-4k60.MOV"
            })
    ));
}

#[test]
fn cli_route_sends_http_page_without_direct_extension_to_yt_dlp_fallback() {
    let (initial_media, startup_error) = resolve_initial_media_argument(
        Some(OsString::from("https://192.0.2.10/media")),
        &AppConfig::default(),
    );

    assert!(startup_error.is_none());
    assert!(matches!(initial_media, Some(InitialMedia::Url(_))));
}

#[test]
fn cli_route_rejects_unsupported_media_protocol() {
    let (initial_media, startup_error) = resolve_initial_media_argument(
        Some(OsString::from("rtsp://192.0.2.10/video.mp4")),
        &AppConfig::default(),
    );

    assert!(initial_media.is_none());
    assert!(
        startup_error
            .as_deref()
            .is_some_and(|error| error.contains("scheme не поддерживается"))
    );
}
