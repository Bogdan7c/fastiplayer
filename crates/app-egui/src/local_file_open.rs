use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use player_core::PreparedMedia;
use pollster::FutureExt as _;
use rustiplayer_config::PlayerDemuxConfig;
use tracing::debug;
use winit::window::Window;

use crate::local_media;

/// Событие фонового открытия локального файла для shell polling-а.
pub(crate) enum LocalFileOpenEvent {
    /// Пользователь выбрал файл, и background thread перешёл к подготовке demuxer-а.
    Preparing { path: PathBuf },

    /// Диалог и подготовка завершились финальным результатом.
    Finished(LocalFileOpenResult),
}

/// Финальный результат async file dialog-а и подготовки локального media.
pub(crate) enum LocalFileOpenResult {
    /// Пользователь закрыл dialog без выбора файла.
    Cancelled,

    /// Файл выбран и успешно подготовлен вне UI thread-а.
    Prepared {
        path: PathBuf,
        prepared_media: PreparedMedia,
    },

    /// Файл выбран, но adapter не смог подготовить demuxer.
    PrepareFailed { path: PathBuf, error: String },

    /// Background job завершился раньше, чем отправил нормальный результат.
    JobFailed { error: String },
}

/// Фоновый job выбора и подготовки локального файла.
pub(crate) struct LocalFileOpenJob {
    /// Receiver событий, которые UI забирает только неблокирующим polling-ом.
    event_rx: Receiver<LocalFileOpenEvent>,

    /// JoinHandle нужен для cleanup после финального результата.
    join_handle: Option<JoinHandle<()>>,
}

impl LocalFileOpenJob {
    /// Запускает async file dialog и подготовку demuxer-а без блокировки UI thread-а.
    pub(crate) fn spawn(
        window: &Window,
        demux_config: PlayerDemuxConfig,
    ) -> std::result::Result<Self, String> {
        let (event_tx, event_rx) = mpsc::channel();
        let file_dialog_future = rfd::AsyncFileDialog::new()
            .set_parent(window)
            .add_filter(
                "Supported Media",
                local_media::SUPPORTED_LOCAL_MEDIA_EXTENSIONS,
            )
            .add_filter("WebM / Matroska", &["webm", "mkv"])
            .add_filter("All Files", &["*"])
            .pick_file();

        let join_handle = thread::Builder::new()
            .name("local-file-open".to_string())
            .spawn(move || {
                let Some(file_handle) = file_dialog_future.block_on() else {
                    send_local_file_open_event(
                        &event_tx,
                        LocalFileOpenEvent::Finished(LocalFileOpenResult::Cancelled),
                    );
                    return;
                };

                let selected_path = file_handle.path().to_path_buf();
                send_local_file_open_event(
                    &event_tx,
                    LocalFileOpenEvent::Preparing {
                        path: selected_path.clone(),
                    },
                );

                let prepare_result = local_media::prepare_local_file(&selected_path, &demux_config)
                    .map(|prepared_media| LocalFileOpenResult::Prepared {
                        path: selected_path.clone(),
                        prepared_media,
                    })
                    .unwrap_or_else(|error| LocalFileOpenResult::PrepareFailed {
                        path: selected_path,
                        error: format!("{error:#}"),
                    });

                send_local_file_open_event(&event_tx, LocalFileOpenEvent::Finished(prepare_result));
            })
            .map_err(|error| format!("Не удалось запустить local file open job: {error}"))?;

        Ok(Self {
            event_rx,
            join_handle: Some(join_handle),
        })
    }

    /// Неблокирующе забирает следующее событие job-а.
    pub(crate) fn try_take_event(&mut self) -> Option<LocalFileOpenEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(LocalFileOpenEvent::Finished(
                LocalFileOpenResult::JobFailed {
                    error: "Local file open job завершился без результата".to_string(),
                },
            )),
        }
    }

    /// Дожидается thread cleanup-а после финального результата без повторного join-а.
    pub(crate) fn join_after_finished(&mut self) -> Option<String> {
        let join_handle = self.join_handle.take()?;

        join_handle
            .join()
            .err()
            .map(|_| "Local file open job завершился panic после результата".to_string())
    }
}

/// Отправляет event в UI и логирует ситуацию, когда receiver уже сброшен.
fn send_local_file_open_event(event_tx: &Sender<LocalFileOpenEvent>, event: LocalFileOpenEvent) {
    if event_tx.send(event).is_err() {
        debug!("UI больше не ждёт результат local file open job-а");
    }
}

/// Форматирует pending overlay после выбора файла.
pub(crate) fn preparing_local_file_message(path: &std::path::Path) -> String {
    format!("Подготовка media-файла: {}", path.display())
}

/// Форматирует shell-level ошибку подготовки локального файла.
pub(crate) fn local_file_prepare_error_message(path: &std::path::Path, error: &str) -> String {
    format!("Ошибка открытия media-файла {}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;

    use super::{
        LocalFileOpenEvent, LocalFileOpenJob, LocalFileOpenResult,
        local_file_prepare_error_message, preparing_local_file_message,
    };

    /// Проверяет, что cancel остаётся shell-level результатом без пути и PreparedMedia.
    #[test]
    fn cancelled_result_is_delivered_without_media() {
        let (event_tx, event_rx) = mpsc::channel();
        event_tx
            .send(LocalFileOpenEvent::Finished(LocalFileOpenResult::Cancelled))
            .expect("test channel должен принять cancel event");
        let mut job = LocalFileOpenJob {
            event_rx,
            join_handle: None,
        };

        let event = job
            .try_take_event()
            .expect("cancel event должен быть готов");

        assert!(matches!(
            event,
            LocalFileOpenEvent::Finished(LocalFileOpenResult::Cancelled)
        ));
    }

    /// Проверяет, что prepare error сохраняет путь для user-facing диагностики.
    #[test]
    fn prepare_error_keeps_selected_path_in_error_message() {
        let path = PathBuf::from("/tmp/broken-media.webm");
        let error_message = local_file_prepare_error_message(&path, "demux failed");

        assert!(error_message.contains("/tmp/broken-media.webm"));
        assert!(error_message.contains("demux failed"));
    }

    /// Проверяет pending-текст подготовки без доступа к state/worker internals.
    #[test]
    fn preparing_message_identifies_selected_file() {
        let path = PathBuf::from("/tmp/clip.mkv");

        assert_eq!(
            preparing_local_file_message(&path),
            "Подготовка media-файла: /tmp/clip.mkv"
        );
    }

    /// Проверяет, что disconnected channel превращается в явную shell error.
    #[test]
    fn disconnected_job_is_reported_as_job_failure() {
        let (event_tx, event_rx) = mpsc::channel();
        drop(event_tx);
        let mut job = LocalFileOpenJob {
            event_rx,
            join_handle: None,
        };

        let event = job
            .try_take_event()
            .expect("disconnect должен дать failure");

        assert!(matches!(
            event,
            LocalFileOpenEvent::Finished(LocalFileOpenResult::JobFailed { .. })
        ));
    }
}
