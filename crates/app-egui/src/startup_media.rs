use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustiplayer_config::AppConfig;
use tracing::{debug, info, warn};

use crate::state::AppState;

/// Интервал polling-а фоновой подготовки YouTube, когда playback ещё не активен.
pub(crate) const YOUTUBE_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Media, которое нужно автоматически открыть после создания окна.
pub(crate) enum InitialMedia {
    /// Локальный файл.
    File(PathBuf),

    /// YouTube/web URL, который нужно подготовить после старта UI.
    YouTubeUrl { url: String },
}

/// Результат фоновой подготовки CLI YouTube URL.
type YoutubeStartupResult = std::result::Result<service_youtube::YoutubeStreamingMedia, String>;

/// Фоновый job, который не блокирует создание окна и UI.
pub(crate) struct YoutubeStartupJob {
    /// URL страницы/ролика, который был передан через CLI.
    source_url: String,

    /// Текст pending-состояния для центрального overlay.
    pending_message: String,

    /// Receiver одноразового результата background resolver-а.
    result_rx: Receiver<YoutubeStartupResult>,

    /// JoinHandle нужен для cleanup после получения результата.
    join_handle: Option<JoinHandle<()>>,
}

impl YoutubeStartupJob {
    /// Запускает подготовку YouTube media на отдельном thread-е.
    pub(crate) fn spawn(
        source_url: String,
        app_config: AppConfig,
    ) -> std::result::Result<Self, String> {
        let (result_tx, result_rx) = mpsc::channel();
        let thread_url = source_url.clone();
        let network_config = app_config.network.clone();
        let youtube_config = app_config.youtube.clone();
        let demux_config = app_config.player.demux;
        let join_handle = thread::Builder::new()
            .name("youtube-startup-resolver".to_string())
            .spawn(move || {
                let resolve_result = service_youtube::open_streaming_media_with_demux_config(
                    &thread_url,
                    &network_config,
                    &youtube_config,
                    &demux_config,
                )
                .map_err(|error| format!("{error:#}"));

                if result_tx.send(resolve_result).is_err() {
                    debug!("UI больше не ждёт результат YouTube startup resolver-а");
                }
            })
            .map_err(|error| format!("Не удалось запустить YouTube startup resolver: {error}"))?;

        Ok(Self {
            source_url,
            pending_message: "Подготовка YouTube stream...".to_string(),
            result_rx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает pending-текст без доступа к внутреннему channel state.
    pub(crate) fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Неблокирующе забирает результат resolver-а, если он уже готов.
    fn try_take_result(&mut self) -> Option<YoutubeStartupResult> {
        let result = match self.result_rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("YouTube startup resolver завершился без результата".to_string())
            }
        };

        if let Some(join_handle) = self.join_handle.take()
            && join_handle.join().is_err()
        {
            return Some(Err(
                "YouTube startup resolver завершился panic после результата".to_string(),
            ));
        }

        Some(result)
    }
}

/// Забирает готовый результат фоновой подготовки YouTube и доставляет его в UI/player.
pub(crate) fn poll_youtube_startup_job(
    job_slot: &mut Option<YoutubeStartupJob>,
    app_state: &mut AppState,
    startup_error_slot: &mut Option<String>,
) {
    let Some(job) = job_slot.as_mut() else {
        return;
    };
    let Some(resolve_result) = job.try_take_result() else {
        return;
    };
    let source_url = job.source_url.clone();
    *job_slot = None;

    match resolve_result {
        Ok(streaming_media) => {
            *startup_error_slot = None;
            info!(
                source = %source_url,
                description = %streaming_media.description,
                "YouTube media подготовлен для streaming playback"
            );
            app_state.load_youtube_demuxer(streaming_media.description, streaming_media.demuxer);
        }
        Err(error) => {
            warn!(source = %source_url, error = %error, "Не удалось подготовить YouTube URL");
            let startup_error = format!("NetworkError: YouTube error: {error}");
            *startup_error_slot = Some(startup_error.clone());
            app_state.set_startup_error(startup_error);
        }
    }
}

/// Подготавливает стартовый media-файл из CLI-аргумента.
///
/// Локальный путь возвращается как файл.
/// HTTP URL открывается через streaming adapter.
pub(crate) fn resolve_initial_media_from_cli(
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    // Берём только первый пользовательский аргумент, чтобы не вводить неполный CLI parser.
    let Some(argument) = std::env::args().nth(1) else {
        return (None, None);
    };

    // URL обрабатываем отдельно: текущий demuxer умеет только локальные файлы.
    if service_youtube::is_probably_url(&argument) {
        info!(url = %argument, "CLI аргумент распознан как YouTube/web URL");
        if !app_config.youtube.enabled {
            return (
                None,
                Some("NetworkError: YouTube adapter отключён в config".to_string()),
            );
        }

        return (Some(InitialMedia::YouTubeUrl { url: argument }), None);
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    (Some(InitialMedia::File(PathBuf::from(argument))), None)
}
