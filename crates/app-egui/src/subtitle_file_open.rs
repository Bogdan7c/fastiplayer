//! Диалог «Load file…» для субтитров: выбор файла в системном окне без блокировки UI.
//!
//! # Архитектура
//! - [`SubtitleFilePicker`] владеет единственным диалогом: пока он открыт, повторное
//!   открытие отклоняется ([`PickerStart::AlreadyOpen`]), поэтому два окна выбора не
//!   появятся даже при быстрых кликах и повторных клавишах.
//! - Системный диалог (`rfd::AsyncFileDialog`) создаётся на UI-потоке (ему нужно родительское
//!   окно), а ждёт ответа рабочий поток; результат приходит через `owner_mailbox` с wake-событием
//!   `AppWakeOwner::Subtitles` — окно просыпается, даже если видео стоит на паузе.
//! - Модуль только выбирает путь. Добавление файла к media и тексты уведомлений — дело
//!   владельца субтитров и `crate::subtitle_message`.
//!
//! # Завершение процесса
//! Диалог может быть открыт в момент выхода из приложения. Join такого потока завис бы,
//! пока пользователь не закроет окно выбора, поэтому `Drop` поток НЕ ждёт: результат
//! отбрасывается, а поток умирает вместе с процессом или раньше, когда диалог закроют.

use std::future::Future;
use std::path::PathBuf;
use std::thread;

use pollster::FutureExt as _;
use tracing::{debug, warn};
use winit::window::Window;

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};

/// Расширения файлов субтитров для фильтра диалога (совпадают с `describe_subtitle_file`).
const SUBTITLE_FILE_DIALOG_EXTENSIONS: &[&str] = &["srt", "vtt", "ass", "ssa"];

/// Имя потока ожидания диалога.
const PICKER_THREAD_NAME: &str = "subtitle-file-picker";

/// Итог одного диалога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubtitleFilePickResult {
    /// Пользователь выбрал файл.
    Picked(PathBuf),
    /// Пользователь закрыл диалог без выбора: молча ничего не делаем.
    Cancelled,
    /// Поток диалога умер, не вернув ответ (паника): считаем отменой, но пишем в лог.
    DialogLost,
}

/// Что получилось при запросе открыть диалог.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerStart {
    /// Диалог открыт.
    Started,
    /// Предыдущий диалог ещё открыт; новый не создавался.
    AlreadyOpen,
    /// Не удалось запустить поток ожидания; причина для лога.
    SpawnFailed { reason: String },
}

/// Владелец диалога выбора файла субтитров.
pub(crate) struct SubtitleFilePicker {
    /// Порт пробуждения UI при готовом ответе.
    wake_port: AppWakePort,
    /// Открытый сейчас диалог, если есть.
    open_dialog: Option<OpenDialog>,
}

/// Ожидание ответа уже открытого диалога.
struct OpenDialog {
    mailbox_receiver: OwnerMailboxReceiver<(), Option<PathBuf>>,
}

impl SubtitleFilePicker {
    /// Создаёт владельца без открытого диалога.
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            open_dialog: None,
        }
    }

    /// Открыт ли диалог прямо сейчас.
    pub(crate) fn is_dialog_open(&self) -> bool {
        self.open_dialog.is_some()
    }

    /// Открывает системный диалог выбора файла субтитров с родителем `parent`.
    pub(crate) fn open_dialog(&mut self, parent: &Window) -> PickerStart {
        if self.is_dialog_open() {
            return PickerStart::AlreadyOpen;
        }
        let dialog = rfd::AsyncFileDialog::new()
            .set_parent(parent)
            .add_filter("Subtitles", SUBTITLE_FILE_DIALOG_EXTENSIONS)
            .add_filter("All Files", &["*"])
            .pick_file();
        self.open_dialog_future(async move { dialog.await.map(|file| file.path().to_path_buf()) })
    }

    /// Запускает ожидание произвольного «диалога». Отдельный вход нужен тестам: настоящее
    /// окно выбора в них недоступно, а вся остальная механика (защита от двух диалогов,
    /// mailbox, wake) должна проверяться боевым кодом.
    pub(crate) fn open_dialog_future(
        &mut self,
        dialog: impl Future<Output = Option<PathBuf>> + Send + 'static,
    ) -> PickerStart {
        if self.is_dialog_open() {
            return PickerStart::AlreadyOpen;
        }
        let (mailbox_publisher, mailbox_receiver) = owner_mailbox(self.wake_port.clone());
        let spawn_result = thread::Builder::new()
            .name(PICKER_THREAD_NAME.to_owned())
            .spawn(move || {
                let picked_path = dialog.block_on();
                match mailbox_publisher.publish_completion(picked_path) {
                    Ok(WakeDelivery::EventLoopClosed) => {
                        debug!("Event loop closed; subtitle picker result will not retry wake");
                    }
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => {
                        warn!("Subtitle file picker published its result twice");
                    }
                }
            });
        match spawn_result {
            Ok(_detached_handle) => {
                self.open_dialog = Some(OpenDialog { mailbox_receiver });
                PickerStart::Started
            }
            Err(error) => PickerStart::SpawnFailed {
                reason: error.to_string(),
            },
        }
    }

    /// Неблокирующе забирает ответ диалога (ровно один раз) и освобождает защиту от повтора.
    pub(crate) fn poll(&mut self) -> Option<SubtitleFilePickResult> {
        let open_dialog = self.open_dialog.as_ref()?;
        let drain = open_dialog.mailbox_receiver.drain();
        let result = if let Some(picked_path) = drain.completion {
            match picked_path {
                Some(path) => SubtitleFilePickResult::Picked(path),
                None => SubtitleFilePickResult::Cancelled,
            }
        } else if drain.producer_disconnected_without_completion {
            warn!("Subtitle file picker thread ended without an answer");
            SubtitleFilePickResult::DialogLost
        } else {
            return None;
        };
        self.open_dialog = None;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app_wake::AppWakeOwner;

    fn picker() -> SubtitleFilePicker {
        SubtitleFilePicker::new(AppWakePort::disconnected(AppWakeOwner::Subtitles))
    }

    /// Ждёт ответ диалога не дольше нескольких секунд.
    fn poll_until_answer(picker: &mut SubtitleFilePicker) -> SubtitleFilePickResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = picker.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "the picker never answered");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn no_dialog_means_nothing_to_poll() {
        let mut picker = picker();
        assert!(!picker.is_dialog_open());
        assert_eq!(picker.poll(), None);
    }

    #[test]
    fn picked_path_is_delivered_exactly_once() {
        let mut picker = picker();
        let path = PathBuf::from("/videos/Movie.rus.srt");
        let expected = path.clone();
        assert_eq!(
            picker.open_dialog_future(std::future::ready(Some(path))),
            PickerStart::Started
        );
        assert_eq!(
            poll_until_answer(&mut picker),
            SubtitleFilePickResult::Picked(expected)
        );
        assert_eq!(picker.poll(), None, "the answer is delivered once");
        assert!(!picker.is_dialog_open());
    }

    #[test]
    fn cancelled_dialog_reports_cancellation() {
        let mut picker = picker();
        picker.open_dialog_future(std::future::ready(None));
        assert_eq!(
            poll_until_answer(&mut picker),
            SubtitleFilePickResult::Cancelled
        );
    }

    #[test]
    fn second_dialog_is_rejected_until_the_first_one_is_answered() {
        let mut picker = picker();
        let (release_dialog, wait_for_release) = mpsc::channel::<()>();
        let first = picker.open_dialog_future(async move {
            // Диалог «открыт», пока тест не отпустит его.
            let _ = wait_for_release.recv();
            None
        });
        assert_eq!(first, PickerStart::Started);
        assert!(picker.is_dialog_open());
        assert_eq!(
            picker.open_dialog_future(std::future::ready(Some(PathBuf::from("/x.srt")))),
            PickerStart::AlreadyOpen
        );
        assert_eq!(picker.poll(), None, "still waiting for the first dialog");

        release_dialog.send(()).expect("dialog thread is alive");
        assert_eq!(
            poll_until_answer(&mut picker),
            SubtitleFilePickResult::Cancelled
        );
        // После ответа новый диалог снова можно открыть.
        assert_eq!(
            picker.open_dialog_future(std::future::ready(None)),
            PickerStart::Started
        );
    }

    #[test]
    fn dialog_thread_that_dies_without_answer_is_reported_as_lost() {
        let mut picker = picker();
        picker.open_dialog_future(async { panic!("simulated dialog crash") });
        assert_eq!(
            poll_until_answer(&mut picker),
            SubtitleFilePickResult::DialogLost
        );
        assert!(!picker.is_dialog_open());
    }
}
