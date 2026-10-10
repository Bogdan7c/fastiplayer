//! Подготовка источника для пересборки media после смены настроек: вне UI-потока.
//!
//! Раньше `prepare_local_file` и подготовка web-источника (сеть, yt-dlp)
//! выполнялись прямо на UI-потоке во время Apply и подвешивали окно. Теперь тяжёлая часть живёт
//! в одном фоновом потоке (`SettingsMediaPrepareJob`, по образцу `LocalFileOpenJob` сессии 18),
//! а UI-поток только опрашивает результат на каждом кадре.
//!
//! Блокирующий путь отката использует ту же функцию `prepare_rebuild_source`, поэтому правила
//! подготовки (кодеки, текст ошибки) существуют в одном месте.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

use fastiplayer_config::PlayerDemuxConfig;
use tracing::warn;

use crate::media_open::{
    ActiveMediaSource, MediaOpenSourceRequest, SafeMediaLabel, SourcePreparationCancel,
    WebMediaOpenRequest,
};
use crate::process_shutdown::{FinishedThreadJoin, ShutdownDeadline, join_thread_until};
use crate::state::PreparedSingleMediaOpen;

/// Сколько ждём завершения потока подготовки после отмены, прежде чем оставить его дорабатывать.
const CANCELLED_PREPARE_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Что именно нужно подготовить заново.
pub(crate) enum RebuildSourceInput {
    /// Локальный файл: читается заново с актуальным demux-конфигом.
    LocalFile {
        path: PathBuf,
        demux: PlayerDemuxConfig,
    },
    /// Web-источник (direct/yt-dlp): запрос уже содержит новые настройки сети и кодеков.
    Web {
        request: Box<WebMediaOpenRequest>,
        safe_label: SafeMediaLabel,
    },
}

/// Полное задание подготовки: источник + порядок предпочтительных кодеков.
pub(crate) struct RebuildPrepareRequest {
    pub(crate) source: RebuildSourceInput,
    pub(crate) preferred_video_codecs: Vec<codec_core::VideoCodec>,
}

/// Результат подготовки: готовый вход strong-open или человеческий текст ошибки.
pub(crate) type RebuildPrepareResult = Result<PreparedSingleMediaOpen, String>;

/// Выполняет подготовку (блокирующую); вызывать только из фонового потока или из отката.
pub(crate) fn prepare_rebuild_source(
    request: RebuildPrepareRequest,
    cancel: &SourcePreparationCancel,
) -> RebuildPrepareResult {
    let RebuildPrepareRequest {
        source,
        preferred_video_codecs,
    } = request;
    match source {
        RebuildSourceInput::LocalFile { path, demux } => {
            match media_source_open::local_media::prepare_local_file(&path, &demux) {
                Ok(prepared_media) => {
                    let prepared_media =
                        prepared_media.with_preferred_video_codecs(&preferred_video_codecs);
                    let source = ActiveMediaSource::LocalFile(path.clone());
                    Ok(PreparedSingleMediaOpen::new(
                        prepared_media,
                        source,
                        SafeMediaLabel::from_local_path(&path),
                    ))
                }
                Err(error) => {
                    warn!(error = %format!("{error:#}"), "Не удалось пересобрать локальный файл после смены настроек");
                    Err(crate::settings_rebuild_message::local_rebuild_failed_message())
                }
            }
        }
        RebuildSourceInput::Web {
            request,
            safe_label,
        } => match cancel.prepare(MediaOpenSourceRequest::Web(*request)) {
            Ok(prepared_open) => {
                let (prepared_media, descriptor) = prepared_open.into_parts();
                let source = descriptor.active_source();
                Ok(PreparedSingleMediaOpen::new(
                    prepared_media.with_preferred_video_codecs(&preferred_video_codecs),
                    source,
                    safe_label,
                )
                .with_descriptor(descriptor))
            }
            Err(error) => {
                warn!(
                    ?error,
                    "Не удалось пересобрать web-источник после смены настроек"
                );
                Err(crate::settings_rebuild_message::web_rebuild_failed_message(
                    error.web_open_failure_reason(),
                ))
            }
        },
    }
}

/// Итог одного опроса фонового потока.
pub(super) enum PrepareJobPoll<T> {
    /// Поток ещё работает.
    Running,
    /// Поток закончил; результат отдаётся ровно один раз.
    Finished(Result<T, String>),
}

/// Один фоновый поток подготовки; результат приходит по каналу.
pub(super) struct SettingsMediaPrepareJob<T> {
    receiver: Receiver<Result<T, String>>,
    join_handle: Option<JoinHandle<()>>,
    cancel: SourcePreparationCancel,
    /// `true`, пока результат подготовки ещё не передан UI.
    ///
    /// Токен отмены разделяют transport/demux подготовленного источника (фоновый seek-поток
    /// progressive HTTP живёт под ним). После выдачи результата токен принадлежит уже
    /// установленному media: отменять его в `Drop` нельзя, иначе перемотка сломается
    /// (`demux seek worker has stopped`).
    result_pending: bool,
}

impl<T: Send + 'static> SettingsMediaPrepareJob<T> {
    /// Запускает произвольную подготовку в отдельном потоке (тесты подставляют свою задачу).
    pub(super) fn spawn_task(
        task: impl FnOnce(&SourcePreparationCancel) -> Result<T, String> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let cancel = SourcePreparationCancel::new();
        let worker_cancel = cancel.clone();
        let join_handle = thread::Builder::new()
            .name("settings-media-prepare".to_string())
            .spawn(move || {
                // Получатель мог исчезнуть (отмена): тогда результат просто некому отдавать.
                let _delivery = sender.send(task(&worker_cancel));
            })
            .map_err(|error| {
                warn!(%error, "Не удалось запустить поток подготовки media для настроек");
                crate::settings_rebuild_message::install_rebuild_failed_message()
            })?;
        Ok(Self {
            receiver,
            join_handle: Some(join_handle),
            cancel,
            result_pending: true,
        })
    }

    /// Неблокирующий опрос: никогда не ждёт поток.
    pub(super) fn poll(&mut self) -> PrepareJobPoll<T> {
        match self.receiver.try_recv() {
            Ok(result) => {
                self.result_pending = false;
                self.reap_finished_thread();
                PrepareJobPoll::Finished(result)
            }
            Err(TryRecvError::Empty) => PrepareJobPoll::Running,
            Err(TryRecvError::Disconnected) => {
                // Поток завершился без результата (panic): это сбой подготовки, а не тишина.
                self.result_pending = false;
                self.reap_finished_thread();
                warn!("Поток подготовки media для настроек завершился без результата");
                PrepareJobPoll::Finished(Err(
                    crate::settings_rebuild_message::install_rebuild_failed_message(),
                ))
            }
        }
    }

    /// Cooperative-отмена: сеть и demux увидят токен, результат будет отброшен.
    pub(super) fn cancel(&mut self) {
        self.cancel.cancel_for_lifecycle();
        let status = join_thread_until(
            &mut self.join_handle,
            ShutdownDeadline::after(CANCELLED_PREPARE_JOIN_TIMEOUT),
        );
        if status == FinishedThreadJoin::StillRunning {
            warn!("Поток подготовки media ещё завершается после отмены; join повторится в Drop");
        }
    }

    fn reap_finished_thread(&mut self) {
        let _status = join_thread_until(
            &mut self.join_handle,
            ShutdownDeadline::after(std::time::Duration::from_millis(50)),
        );
    }
}

impl<T> Drop for SettingsMediaPrepareJob<T> {
    fn drop(&mut self) {
        // Fail-safe: пока результат не выдан, поток не должен пережить владельца; токен
        // отменяется только в этом случае (см. `result_pending`).
        if self.result_pending {
            self.cancel.cancel_for_lifecycle();
        }
        let _status = join_thread_until(
            &mut self.join_handle,
            ShutdownDeadline::after(CANCELLED_PREPARE_JOIN_TIMEOUT),
        );
    }
}

impl SettingsMediaPrepareJob<PreparedSingleMediaOpen> {
    /// Production-запуск: подготовка источника из задания настроек.
    pub(super) fn spawn(request: RebuildPrepareRequest) -> Result<Self, String> {
        Self::spawn_task(move |cancel| prepare_rebuild_source(request, cancel))
    }
}

#[cfg(test)]
#[path = "prepare/tests.rs"]
mod tests;
