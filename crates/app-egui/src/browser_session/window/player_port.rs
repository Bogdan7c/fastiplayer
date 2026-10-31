//! Настоящий плеер окна для водителя: `AppState` окна и то, что ему нужно для
//! открытия медиа (координатор открытия в `PlaylistRuntime`, рендер).
//!
//! Медиа поколения потока идёт обычным путём открытия `AppState`
//! (`begin_prepared_media_strong` → `poll_prepared_media_strong`), как файл из Open:
//! декодер выбирает плеер по возможностям рендера окна (VA-API без копий, 10 бит,
//! HW→SW по правилам `[video]` настроек) — без своего кода выбора.
//!
//! Источник — [`ActiveMediaSource::BrowserStream`]: без адреса и без файла, поэтому
//! пересборка после настроек и возобновление его не открывают заново, памяти дорожек
//! и сайдкаров субтитров у него нет. Метка — нейтральная «browser stream»: ни домена,
//! ни заголовка страницы (инвариант 7: журнал лежит на диске).

use std::sync::Arc;

use media_source_open::safe_media_label::SafeMediaLabel;
use player_core::{
    MediaInstallCancellationCause, MediaInstallCompletion, PlaybackIntent, PlayerCommand,
    PlayerErrorKind, PlayerSnapshot, PlayerWorkerSendError, PreparedMedia,
};
use render_shell_api::RenderShell;
use tracing::warn;

use super::super::playback::BrowserPlaybackFault;
use super::driver::{BeginLoad, LoadPoll, WindowPlayer};
use crate::downloaded_range::DownloadedRange;
use crate::media_open::{MediaOpenTerminalOutcome, PlayerInstallFailureReason};
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::{
    ActiveMediaSource, AppState, PreparedSingleMediaOpen, StrongMediaOpenError,
    StrongMediaOpenPoll, TappedPlayerEvents,
};
use crate::telemetry::Telemetry;

/// Метка медиа браузерного потока в интерфейсе и журнале (без данных страницы).
const BROWSER_STREAM_LABEL: &str = "browser stream";

/// `AppState` окна и его соседи на время одного прохода водителя.
pub(super) struct AppStateWindowPlayer<'window> {
    pub(super) app_state: &'window mut AppState,
    pub(super) playlist_runtime: &'window mut PlaylistRuntime,
    pub(super) renderer: &'window mut dyn RenderShell,
    pub(super) telemetry: &'window Telemetry,
}

impl WindowPlayer for AppStateWindowPlayer<'_> {
    fn has_pending_load(&self) -> bool {
        self.app_state.has_pending_prepared_media_strong()
    }

    fn begin_load(&mut self, prepared: PreparedMedia, intent: PlaybackIntent) -> BeginLoad {
        // Место старта поколения решил владелец источника: страница назвала позицию, и
        // `SessionMedia` уже перемотал поток (03e). Путь открытия принимает готовую
        // позицию плеера как есть — цели восстановления очереди у потока нет.
        let input = PreparedSingleMediaOpen::prepared_by_source(
            prepared,
            ActiveMediaSource::BrowserStream,
            SafeMediaLabel::from_service_safe_label(BROWSER_STREAM_LABEL),
        );
        match self.app_state.begin_prepared_media_strong(
            self.playlist_runtime,
            self.renderer,
            input,
            intent,
        ) {
            Ok(()) => BeginLoad::Started,
            Err(error) => {
                // Текст ошибки открытия не несёт данных страницы (метка нейтральная).
                warn!(%error, "The browser window could not start installing the media");
                BeginLoad::Refused(fault_for_open_failure(&error).unwrap_or(
                    BrowserPlaybackFault::PlaybackFailed(PlayerErrorKind::RuntimeError),
                ))
            }
        }
    }

    fn offer_downloaded_range(&mut self, range: DownloadedRange) {
        self.app_state.note_downloaded_range(range);
    }

    fn supersede_pending_load(&mut self) {
        if let Err(error) = self
            .app_state
            .supersede_pending_prepared_media_strong(self.playlist_runtime)
        {
            // Не отменилось — значит установка кончится сама; водитель дождётся итога.
            warn!(%error, "The browser window could not cancel a replaced media install");
        }
    }

    fn poll_load(&mut self) -> LoadPoll {
        if !self.app_state.has_pending_prepared_media_strong() {
            // Слот уже свободен без итога — так не бывает; не висим, а считаем отменой
            // (отмена текущей загрузки — её отказ, это решает учёт загрузок водителя).
            warn!("The browser window media install vanished without a result");
            return LoadPoll::Cancelled;
        }
        match self
            .app_state
            .poll_prepared_media_strong(self.playlist_runtime, self.renderer.video_frames())
        {
            StrongMediaOpenPoll::Pending => LoadPoll::Pending,
            StrongMediaOpenPoll::Installed(installed) => {
                // Как у Open файла: субтитры потока, кэш кадра, «что сейчас играет».
                self.app_state.record_installed_media(&installed);
                match installed.completion {
                    MediaInstallCompletion::Installed {
                        media_instance_id, ..
                    } => LoadPoll::Installed(media_instance_id),
                    // Путь открытия отдаёт `Installed` только с установленным
                    // экземпляром; иное — нарушение, честнее назвать это отказом.
                    _ => LoadPoll::Failed(BrowserPlaybackFault::PlaybackFailed(
                        PlayerErrorKind::RuntimeError,
                    )),
                }
            }
            StrongMediaOpenPoll::Failed(error) => match fault_for_open_failure(&error) {
                None => LoadPoll::Cancelled,
                Some(fault) => {
                    warn!(%error, "The browser window media install failed");
                    LoadPoll::Failed(fault)
                }
            },
        }
    }

    fn send(&mut self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.app_state.player_command_sender().try_send(command)
    }

    fn snapshot(&mut self) -> Arc<PlayerSnapshot> {
        self.app_state.refresh_player_snapshot()
    }

    fn take_player_events(&mut self) -> TappedPlayerEvents {
        self.app_state.take_tapped_player_events()
    }

    fn video_frames_presented(&self) -> u64 {
        self.telemetry.video_frames_presented()
    }
}

/// Отказ загрузки по ошибке пути открытия; `None` — открытие отменено (любой причиной).
///
/// Отмена здесь не классифицируется: заменённой загрузке она — норма, текущей — отказ
/// (`InstallAborted`), а какая загрузка текущая, знает только учёт загрузок водителя.
fn fault_for_open_failure(error: &StrongMediaOpenError) -> Option<BrowserPlaybackFault> {
    let fault = match error {
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled { cause, .. }) => {
            log_cancellation(*cause);
            return None;
        }
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerFailed {
            reason, ..
        }) => match reason {
            PlayerInstallFailureReason::UnsupportedVideoFormat
            | PlayerInstallFailureReason::NoSuitableVideoDecoder => {
                BrowserPlaybackFault::UnsupportedVideo
            }
            PlayerInstallFailureReason::UnsupportedAudioFormat => {
                BrowserPlaybackFault::PlaybackFailed(PlayerErrorKind::UnsupportedAudioCodec)
            }
            _ => BrowserPlaybackFault::PlaybackFailed(PlayerErrorKind::RuntimeError),
        },
        StrongMediaOpenError::RendererRejectedVideoImport(_) => {
            BrowserPlaybackFault::VideoDecoderStartFailed
        }
        StrongMediaOpenError::PostInstalledCompensated { failure, .. } => {
            return fault_for_open_failure(failure);
        }
        _ => BrowserPlaybackFault::PlaybackFailed(PlayerErrorKind::RuntimeError),
    };
    Some(fault)
}

/// Причина отмены — в журнал на уровне debug; отказ ли это, водитель пишет сам.
fn log_cancellation(cause: MediaInstallCancellationCause) {
    tracing::debug!(?cause, "Browser window media install was cancelled");
}
