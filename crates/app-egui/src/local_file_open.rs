use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use player_core::PreparedMedia;
use pollster::FutureExt as _;
use rustiplayer_config::PlayerDemuxConfig;
use tracing::{debug, warn};
use winit::window::Window;

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::local_media;

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

/// Один UI drain latest progress и exactly-once completion.
pub(crate) struct LocalFileOpenDrain {
    /// Последний выбранный path, для которого background job начал preparation.
    pub(crate) preparing_path: Option<PathBuf>,
    /// Финальный результат, если worker уже завершил job.
    pub(crate) completion: Option<LocalFileOpenResult>,
}

impl LocalFileOpenDrain {
    /// `true` означает реальную видимую мутацию shell status/media state.
    pub(crate) fn has_payload(&self) -> bool {
        self.preparing_path.is_some() || self.completion.is_some()
    }
}

/// Фоновый job выбора и подготовки локального файла.
pub(crate) struct LocalFileOpenJob {
    /// Owner mailbox хранит latest path отдельно от lossless terminal result.
    mailbox_receiver: OwnerMailboxReceiver<PathBuf, LocalFileOpenResult>,

    /// JoinHandle нужен для cleanup после финального результата.
    join_handle: Option<JoinHandle<()>>,
}

impl LocalFileOpenJob {
    /// Запускает async file dialog и подготовку demuxer-а без блокировки UI thread-а.
    pub(crate) fn spawn(
        window: &Window,
        demux_config: PlayerDemuxConfig,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (mailbox_publisher, mailbox_receiver) = owner_mailbox(wake_port);
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
                    publish_local_file_completion(
                        &mailbox_publisher,
                        LocalFileOpenResult::Cancelled,
                    );
                    return;
                };

                let selected_path = file_handle.path().to_path_buf();
                let progress_outcome = mailbox_publisher.publish_progress(selected_path.clone());
                if progress_outcome.wake_delivery == WakeDelivery::EventLoopClosed {
                    debug!("Event loop закрыт; local-file progress оставлен без wake retry");
                }

                let prepare_result = local_media::prepare_local_file(&selected_path, &demux_config)
                    .map(|prepared_media| LocalFileOpenResult::Prepared {
                        path: selected_path.clone(),
                        prepared_media,
                    })
                    .unwrap_or_else(|error| LocalFileOpenResult::PrepareFailed {
                        path: selected_path,
                        error: format!("{error:#}"),
                    });

                publish_local_file_completion(&mailbox_publisher, prepare_result);
            })
            .map_err(|error| format!("Не удалось запустить local file open job: {error}"))?;

        Ok(Self {
            mailbox_receiver,
            join_handle: Some(join_handle),
        })
    }

    /// Неблокирующе забирает оба независимых slot-а и никогда не ждёт join.
    pub(crate) fn drain(&mut self) -> LocalFileOpenDrain {
        let mailbox_drain = self.mailbox_receiver.drain();
        let mut completion = mailbox_drain.completion;

        if completion.is_none()
            && (mailbox_drain.producer_disconnected_without_completion
                || self
                    .join_handle
                    .as_ref()
                    .is_some_and(JoinHandle::is_finished))
        {
            completion = Some(LocalFileOpenResult::JobFailed {
                error: self
                    .take_finished_join_error()
                    .unwrap_or_else(|| "Local file open job завершился без результата".to_string()),
            });
        } else if completion.is_some() {
            // Publish выполняется перед return worker-а. JoinHandle может ещё не успеть
            // стать finished, поэтому UI либо join-ит готовый thread, либо detach-ит его.
            if let Some(join_error) = self.take_finished_join_error() {
                completion = Some(LocalFileOpenResult::JobFailed { error: join_error });
            }
            self.join_handle = None;
        }

        LocalFileOpenDrain {
            preparing_path: mailbox_drain.latest_progress,
            completion,
        }
    }

    /// Join-ит только уже завершившийся thread, поэтому UI callback не блокируется.
    fn take_finished_join_error(&mut self) -> Option<String> {
        let join_handle = self.join_handle.take()?;
        if !join_handle.is_finished() {
            self.join_handle = Some(join_handle);
            return None;
        }
        join_handle
            .join()
            .err()
            .map(|_| "Local file open job завершился panic после результата".to_string())
    }
}

/// Публикует terminal без blocking send; повторный terminal является invariant error.
fn publish_local_file_completion(
    publisher: &crate::app_wake::OwnerMailboxPublisher<PathBuf, LocalFileOpenResult>,
    completion: LocalFileOpenResult,
) {
    match publisher.publish_completion(completion) {
        Ok(WakeDelivery::EventLoopClosed) => {
            debug!("Event loop закрыт; local-file terminal оставлен без wake retry");
        }
        Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
        Err(CompletionPublishError::AlreadyPublished) => {
            warn!("Local file open job попытался опубликовать второй terminal result");
        }
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

    use super::{
        LocalFileOpenDrain, LocalFileOpenResult, local_file_prepare_error_message,
        preparing_local_file_message,
    };

    /// Проверяет, что terminal cancel считается видимой exactly-once мутацией.
    #[test]
    fn cancelled_result_is_visible_completion_without_media() {
        let drain = LocalFileOpenDrain {
            preparing_path: None,
            completion: Some(LocalFileOpenResult::Cancelled),
        };

        assert!(drain.has_payload());
        assert!(matches!(
            drain.completion,
            Some(LocalFileOpenResult::Cancelled)
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

    /// Пустой queued wake event не должен объявлять видимую мутацию.
    #[test]
    fn empty_drain_is_not_visible_mutation() {
        let drain = LocalFileOpenDrain {
            preparing_path: None,
            completion: None,
        };

        assert!(!drain.has_payload());
    }
}
