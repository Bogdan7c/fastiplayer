//! Вложенные шрифты контейнера (MKV): отбор файлов-шрифтов, фоновая подготовка и установка
//! в растеризатор владельца.
//!
//! # Поток данных
//! Труба субтитров приносит `MediaStarted` с вложениями media. Владелец:
//! 1. отбирает только файлы-шрифты ([`select_font_attachments`]), остальное считает и пишет
//!    в лог числом (без имён и путей);
//! 2. отдаёт их фоновой задаче ([`AttachedFontsJob`]): разбор файлов шрифтов тяжёлый
//!    (`PreparedAttachedFonts::prepare`), на потоке UI ему не место;
//! 3. готовый результат устанавливает в растеризатор дешёвой фазой. Если растеризатора ещё нет
//!    (библиотека шрифтов строится в фоне), результат ждёт его появления.
//!
//! # Жизненный цикл
//! Шрифты принадлежат ОДНОМУ media: при смене/сбросе media задача отменяется (результат
//! выбрасывается), ожидающий результат забывается, а установленные шрифты снимаются
//! (`clear_attached_fonts`) — встроенные и системные шрифты при этом остаются.
//!
//! Задача привязана к `MediaInstanceId`: результат чужого media не устанавливается никогда.

use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use bytes::Bytes;
use media_core::{MediaAttachment, MediaAttachments, SkippedAttachmentReason};
use player_core::MediaInstanceId;
use subtitle_raster::{AttachedFontLimits, PreparedAttachedFonts};

use super::SubtitleOwner;
use crate::app_wake::{AppWakePort, OwnerMailboxReceiver, WakeDelivery, owner_mailbox};

/// Имя потока для диагностики.
const ATTACHED_FONTS_THREAD_NAME: &str = "subtitle-attached-fonts";

/// MIME-типы, под которыми Matroska хранит шрифты (кроме любого `font/*`).
const FONT_MEDIA_TYPES: [&str; 4] = [
    "application/x-truetype-font",
    "application/vnd.ms-opentype",
    "application/font-sfnt",
    "application/x-font-ttf",
];

/// Расширения файлов шрифтов (без точки, нижний регистр) для вложений без внятного MIME.
const FONT_FILE_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// Результат отбора вложений: что отдать на подготовку и что отбросили.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FontAttachmentSelection {
    /// Содержимое файлов-шрифтов (клон `Bytes` дешёвый: данные не копируются).
    pub(super) fonts: Vec<Bytes>,
    /// Вложения, не являющиеся шрифтами (обложки, заметки…).
    pub(super) not_fonts: usize,
}

/// Файл похож на шрифт по MIME-типу или расширению имени.
fn is_font_like(attachment: &MediaAttachment) -> bool {
    if let Some(media_type) = attachment.media_type.as_deref() {
        let media_type = media_type.trim().to_ascii_lowercase();
        if media_type.starts_with("font/") || FONT_MEDIA_TYPES.contains(&media_type.as_str()) {
            return true;
        }
    }
    attachment
        .file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| {
            FONT_FILE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

/// Отбирает из вложений media только шрифты.
pub(super) fn select_font_attachments(attachments: &MediaAttachments) -> FontAttachmentSelection {
    let mut selection = FontAttachmentSelection::default();
    for attachment in &attachments.attachments {
        if is_font_like(attachment) {
            selection.fonts.push(attachment.bytes.clone());
        } else {
            selection.not_fonts += 1;
        }
    }
    selection
}

/// Сколько вложений demuxer пропустил по каждой причине (для лога).
#[derive(Debug, Default, PartialEq, Eq)]
struct SkippedCounts {
    too_large: usize,
    total_limit_exceeded: usize,
    unreadable: usize,
}

fn count_skipped(attachments: &MediaAttachments) -> SkippedCounts {
    let mut counts = SkippedCounts::default();
    for skipped in &attachments.skipped {
        match skipped.reason {
            SkippedAttachmentReason::TooLarge { .. } => counts.too_large += 1,
            SkippedAttachmentReason::TotalLimitExceeded { .. } => counts.total_limit_exceeded += 1,
            SkippedAttachmentReason::Unreadable => counts.unreadable += 1,
        }
    }
    counts
}

/// Фоновая подготовка шрифтов одного media.
pub(super) struct AttachedFontsJob {
    media_instance_id: MediaInstanceId,
    mailbox_receiver: OwnerMailboxReceiver<(), PreparedAttachedFonts>,
    join_handle: Option<JoinHandle<()>>,
}

/// Что владелец увидел при опросе задачи.
pub(super) enum AttachedFontsPoll {
    /// Ещё работает.
    Pending,
    /// Готово: подготовленные шрифты. `None` — поток умер без результата.
    Finished(Option<PreparedAttachedFonts>),
}

impl AttachedFontsJob {
    /// Запускает подготовку. Ошибка создания потока возвращается видом ошибки ввода-вывода.
    fn spawn(
        media_instance_id: MediaInstanceId,
        fonts: Vec<Bytes>,
        limits: AttachedFontLimits,
        wake_port: AppWakePort,
    ) -> Result<Self, io::ErrorKind> {
        let (publisher, mailbox_receiver) = owner_mailbox(wake_port);
        let join_handle = thread::Builder::new()
            .name(ATTACHED_FONTS_THREAD_NAME.to_owned())
            .spawn(move || {
                // Копия байтов в `Arc<[u8]>` тоже тяжёлая, поэтому делается здесь, в фоне.
                let files = fonts
                    .iter()
                    .map(|font| Arc::<[u8]>::from(font.as_ref()))
                    .collect::<Vec<_>>();
                let prepared = PreparedAttachedFonts::prepare(files, &limits);
                match publisher.publish_completion(prepared) {
                    Ok(WakeDelivery::EventLoopClosed) => {
                        tracing::debug!("Event loop closed; attached fonts result is dropped");
                    }
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(error) => {
                        tracing::error!(?error, "Attached fonts job published its result twice");
                    }
                }
            })
            .map_err(|error| error.kind())?;
        Ok(Self {
            media_instance_id,
            mailbox_receiver,
            join_handle: Some(join_handle),
        })
    }

    /// Неблокирующий опрос.
    fn poll(&self) -> AttachedFontsPoll {
        let drain = self.mailbox_receiver.drain();
        if let Some(prepared) = drain.completion {
            return AttachedFontsPoll::Finished(Some(prepared));
        }
        if drain.producer_disconnected_without_completion {
            return AttachedFontsPoll::Finished(None);
        }
        AttachedFontsPoll::Pending
    }

    /// Поток завершился (освобождение не блокирует).
    fn is_finished(&self) -> bool {
        self.join_handle
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for AttachedFontsJob {
    fn drop(&mut self) {
        // Подготовка ограничена лимитами конфига и не отменяется на ходу; владелец отпускает
        // задачу в `retiring`, пока она не завершится, поэтому здесь ожидание почти нулевое.
        if let Some(join_handle) = self.join_handle.take()
            && join_handle.join().is_err()
        {
            tracing::warn!("Attached fonts thread panicked");
        }
    }
}

/// Подготовленные шрифты, ждущие появления растеризатора.
struct ReadyAttachedFonts {
    media_instance_id: MediaInstanceId,
    prepared: PreparedAttachedFonts,
    limits: AttachedFontLimits,
}

/// Состояние вложенных шрифтов владельца.
#[derive(Default)]
pub(super) struct AttachedFontsState {
    /// Идущая подготовка шрифтов текущего media.
    job: Option<(AttachedFontsJob, AttachedFontLimits)>,
    /// Готовый результат, ждущий растеризатора.
    waiting_for_rasterizer: Option<ReadyAttachedFonts>,
    /// Отменённые задачи: ждём завершения потоков без блокировки UI.
    retiring: Vec<AttachedFontsJob>,
}

impl AttachedFontsState {
    /// Идёт ли работа, ради которой владельцу нужны wake-события.
    pub(super) fn has_running_work(&self) -> bool {
        self.job.is_some() || !self.retiring.is_empty()
    }

    /// Есть ли готовый результат, ждущий растеризатора (владельцу пора заказать его).
    pub(super) fn is_waiting_for_rasterizer(&self) -> bool {
        self.waiting_for_rasterizer.is_some()
    }
}

impl SubtitleOwner {
    /// Вложения нового media: шрифты уходят на фоновую подготовку.
    ///
    /// Вызывается для сообщения `MediaStarted` ТЕКУЩЕГО media (чужое отброшено маршрутизацией).
    pub(super) fn start_attached_fonts(
        &mut self,
        media_instance_id: MediaInstanceId,
        attachments: &MediaAttachments,
    ) {
        let skipped = count_skipped(attachments);
        let selection = select_font_attachments(attachments);
        if skipped != SkippedCounts::default() || selection.not_fonts > 0 {
            // Имён и путей в логе нет: только числа и причины.
            tracing::debug!(
                skipped_too_large = skipped.too_large,
                skipped_total_limit = skipped.total_limit_exceeded,
                skipped_unreadable = skipped.unreadable,
                not_fonts = selection.not_fonts,
                "Some container attachments were not used as subtitle fonts"
            );
        }
        if skipped.too_large + skipped.total_limit_exceeded > 0 {
            tracing::warn!(
                too_large = skipped.too_large,
                over_total_limit = skipped.total_limit_exceeded,
                "Attached fonts over the size limits were skipped; subtitles use other fonts"
            );
        }
        if selection.fonts.is_empty() {
            return;
        }
        let limits = self.settings.embedded_limits.attached_fonts;
        let font_count = selection.fonts.len();
        match AttachedFontsJob::spawn(
            media_instance_id,
            selection.fonts,
            limits,
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                tracing::debug!(fonts = font_count, "Preparing attached subtitle fonts");
                self.attached_fonts.job = Some((job, limits));
            }
            Err(kind) => {
                tracing::warn!(?kind, "Attached fonts thread could not be started");
            }
        }
    }

    /// Опрос подготовки шрифтов. `true` — видимое состояние могло измениться.
    pub(super) fn poll_attached_fonts(&mut self) -> bool {
        // Результаты отменённых задач выбрасываем, завершённые потоки освобождаем.
        self.attached_fonts.retiring.retain(|job| {
            let _discarded = job.poll();
            !job.is_finished()
        });
        let Some((job, limits)) = &self.attached_fonts.job else {
            return false;
        };
        let AttachedFontsPoll::Finished(result) = job.poll() else {
            return false;
        };
        let (media_instance_id, limits) = (job.media_instance_id, *limits);
        self.attached_fonts.job = None;

        let is_current = self
            .media
            .as_ref()
            .is_some_and(|media| media.media_instance_id == media_instance_id);
        if !is_current {
            tracing::debug!("Dropped attached fonts prepared for media that is no longer current");
            return false;
        }
        let Some(prepared) = result else {
            tracing::warn!("Attached fonts thread stopped without a result");
            return false;
        };
        self.attached_fonts.waiting_for_rasterizer = Some(ReadyAttachedFonts {
            media_instance_id,
            prepared,
            limits,
        });
        let installed = self.install_ready_attached_fonts();
        if self.attached_fonts.is_waiting_for_rasterizer() {
            // Шрифты есть, а растеризатора нет (нет файла рядом, который обычно строит
            // библиотеку): заказываем его, установка произойдёт при его появлении.
            self.request_rasterizer_in_background();
        }
        installed
    }

    /// Устанавливает готовые шрифты, если растеризатор уже есть. `true` — установлено.
    pub(super) fn install_ready_attached_fonts(&mut self) -> bool {
        let Some(ready) = self.attached_fonts.waiting_for_rasterizer.take() else {
            return false;
        };
        let ReadyAttachedFonts {
            media_instance_id,
            prepared,
            limits,
        } = ready;
        match self.presenter.install_attached_fonts(prepared, &limits) {
            // Отчёт об установке придёт от потока событием и будет записан в лог при приёме.
            Ok(()) => true,
            Err(prepared) => {
                // Растеризатора ещё нет: ждём его (владелец заказывает библиотеку шрифтов).
                self.attached_fonts.waiting_for_rasterizer = Some(ReadyAttachedFonts {
                    media_instance_id,
                    prepared,
                    limits,
                });
                false
            }
        }
    }

    /// Смена/сброс media: шрифты прежнего media забываются и снимаются с растеризатора.
    pub(super) fn forget_attached_fonts(&mut self) {
        if let Some((job, _limits)) = self.attached_fonts.job.take() {
            self.attached_fonts.retiring.push(job);
        }
        self.attached_fonts.waiting_for_rasterizer = None;
        // Итог снятия придёт от потока событием и будет записан в лог при приёме.
        self.presenter.clear_attached_fonts();
    }
}

#[cfg(test)]
mod tests {
    use media_core::SkippedAttachment;

    use super::*;

    fn attachment(name: &str, media_type: Option<&str>) -> MediaAttachment {
        MediaAttachment {
            file_name: name.to_owned(),
            media_type: media_type.map(str::to_owned),
            bytes: Bytes::from_static(b"data"),
        }
    }

    #[test]
    fn fonts_are_recognised_by_media_type_or_extension_and_others_are_counted() {
        let attachments = MediaAttachments {
            attachments: vec![
                attachment("a.bin", Some("font/ttf")),
                attachment("b.bin", Some("Application/X-TrueType-Font")),
                attachment("c.bin", Some("application/vnd.ms-opentype")),
                attachment("d.bin", Some("application/font-sfnt")),
                attachment("Arial.TTF", None),
                attachment("e.otf", Some("application/octet-stream")),
                attachment("f.ttc", None),
                attachment("g.otc", None),
                attachment("cover.jpg", Some("image/jpeg")),
                attachment("notes", None),
            ],
            skipped: Vec::new(),
        };

        let selection = select_font_attachments(&attachments);

        assert_eq!(selection.fonts.len(), 8);
        assert_eq!(selection.not_fonts, 2);
    }

    #[test]
    fn skipped_attachments_are_counted_per_reason() {
        let attachments = MediaAttachments {
            attachments: Vec::new(),
            skipped: vec![
                SkippedAttachment {
                    file_name: "x.ttf".to_owned(),
                    reason: SkippedAttachmentReason::TooLarge {
                        size_bytes: 10,
                        limit_bytes: 5,
                    },
                },
                SkippedAttachment {
                    file_name: "y.ttf".to_owned(),
                    reason: SkippedAttachmentReason::Unreadable,
                },
            ],
        };

        assert_eq!(
            count_skipped(&attachments),
            SkippedCounts {
                too_large: 1,
                total_limit_exceeded: 0,
                unreadable: 1
            }
        );
    }
}
