use std::path::{Path, PathBuf};
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
struct YoutubeStartupJob {
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
    fn spawn(source_url: String, app_config: AppConfig) -> std::result::Result<Self, String> {
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
    fn pending_message(&self) -> &str {
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

/// Владеет shell-состоянием стартового media без знания о renderer/GPU.
pub(crate) struct StartupMediaController {
    /// Media, переданное через CLI или восстановленное после suspend.
    initial_media: Option<InitialMedia>,

    /// Фоновая подготовка CLI YouTube URL, если она уже запущена.
    youtube_startup_job: Option<YoutubeStartupJob>,

    /// Startup-ошибка shell-слоя, которую нужно показать после создания UI.
    startup_error: Option<String>,
}

impl StartupMediaController {
    /// Создаёт controller из startup-состояния, которое было собрано до запуска окна.
    pub(crate) fn new(initial_media: Option<InitialMedia>, startup_error: Option<String>) -> Self {
        Self {
            initial_media,
            youtube_startup_job: None,
            startup_error,
        }
    }

    /// Возвращает копию startup-ошибки для инициализации `AppState`.
    pub(crate) fn startup_error_message(&self) -> Option<String> {
        self.startup_error.clone()
    }

    /// Возвращает pending-текст активного YouTube startup job.
    pub(crate) fn pending_message(&self) -> Option<&str> {
        self.youtube_startup_job
            .as_ref()
            .map(YoutubeStartupJob::pending_message)
    }

    /// Сообщает shell scheduler-у, нужно ли продолжать polling YouTube startup job.
    pub(crate) fn has_pending_youtube_job(&self) -> bool {
        self.youtube_startup_job.is_some()
    }

    /// Запоминает текущий локальный файл для повторного открытия после следующего resume.
    pub(crate) fn restore_file_on_next_resume(&mut self, path: &Path) {
        self.initial_media = Some(InitialMedia::File(path.to_path_buf()));
        self.startup_error = None;
    }

    /// Запускает отложенное стартовое media после того, как `AppState` уже создан.
    pub(crate) fn start_pending_initial_media(
        &mut self,
        app_state: &mut AppState,
        app_config: &AppConfig,
    ) {
        if let Some(pending_message) = self.pending_message() {
            app_state.set_startup_pending(pending_message.to_string());
        }

        let Some(initial_media) = self.initial_media.take() else {
            return;
        };

        match initial_media {
            InitialMedia::File(path) => {
                info!(path = %path.display(), "Автозагрузка файла из CLI");
                app_state.load_file(&path);
            }
            InitialMedia::YouTubeUrl { url } => {
                info!(source = %url, "Автозагрузка YouTube URL из CLI");
                self.start_youtube_startup_job(url, app_state, app_config);
            }
        }
    }

    /// Неблокирующе опрашивает результат фоновой подготовки YouTube URL.
    pub(crate) fn poll_youtube_job(&mut self, app_state: &mut AppState) {
        poll_youtube_startup_job(
            &mut self.youtube_startup_job,
            app_state,
            &mut self.startup_error,
        );
    }

    /// Запускает background resolve для CLI YouTube URL и сразу обновляет UI state.
    fn start_youtube_startup_job(
        &mut self,
        source_url: String,
        app_state: &mut AppState,
        app_config: &AppConfig,
    ) {
        app_state.set_startup_pending("Подготовка YouTube stream...".to_string());
        match YoutubeStartupJob::spawn(source_url, app_config.clone()) {
            Ok(job) => {
                self.startup_error = None;
                self.youtube_startup_job = Some(job);
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить YouTube startup resolver");
                let startup_error = format!("NetworkError: YouTube error: {error}");
                self.startup_error = Some(startup_error.clone());
                app_state.set_startup_error(startup_error);
            }
        }
    }
}

/// Забирает готовый результат фоновой подготовки YouTube и доставляет его в UI/player.
fn poll_youtube_startup_job(
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn controller_exposes_startup_error_for_app_state_creation() {
        let controller = StartupMediaController::new(None, Some("startup failure".to_string()));

        assert_eq!(
            controller.startup_error_message(),
            Some("startup failure".to_string())
        );
    }

    #[test]
    fn restore_file_on_next_resume_replaces_initial_media_and_clears_error() {
        let restored_path = Path::new("/tmp/restored.webm");
        let mut controller = StartupMediaController::new(
            Some(InitialMedia::YouTubeUrl {
                url: "https://example.test/video".to_string(),
            }),
            Some("old error".to_string()),
        );

        controller.restore_file_on_next_resume(restored_path);

        assert!(controller.startup_error_message().is_none());
        assert!(matches!(
            controller.initial_media.as_ref(),
            Some(InitialMedia::File(path)) if path.as_path() == restored_path
        ));
    }

    #[test]
    fn pending_message_reports_existing_youtube_job() {
        let (_result_tx, result_rx) = mpsc::channel();
        let controller = StartupMediaController {
            initial_media: None,
            youtube_startup_job: Some(YoutubeStartupJob {
                source_url: "https://example.test/video".to_string(),
                pending_message: "Подготовка YouTube stream...".to_string(),
                result_rx,
                join_handle: None,
            }),
            startup_error: None,
        };

        assert!(controller.has_pending_youtube_job());
        assert_eq!(
            controller.pending_message(),
            Some("Подготовка YouTube stream...")
        );
    }
}
