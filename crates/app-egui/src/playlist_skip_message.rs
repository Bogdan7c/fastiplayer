//! Тексты для пользователя о пропуске битых файлов очереди (UX edge cases, сессия 07).
//!
//! Controller плейлиста сообщает типизированный факт ([`AutomaticQueueNotice`]): что было
//! пропущено и чем закончилась цепочка. Здесь — только формулировки и выбор вида показа;
//! жизненный цикл плашек остаётся у `NotificationCenter`, маршрут —
//! `AppState::show_playlist_queue_notices` (`state/notification_routing.rs`).
//!
//! Решения владельца:
//! - пропуск с продолжением — информационная плашка (8 с): «Пропущено 2 файла: a.mkv, b.mp4»;
//!   до трёх имён, остальные — «и ещё K»; имя без пути к папке;
//! - очередь не открылась целиком (до этого ничего не играло) — ошибка в центре до ×;
//! - если до этого что-то играло — обычная плашка «… Дальше в очереди ничего нет»;
//! - явно выбранная политика «остановиться» — временная плашка (5 с) с причиной.

use crate::playlist_runtime::{AutomaticQueueNotice, QueueBlockReason, SkippedItemsSummary};

/// Текст, когда не открылся ни один элемент очереди.
pub(crate) const NOTHING_IN_QUEUE_OPENED_MESSAGE: &str = "Could not open any file in the queue";

/// Очередь встала, потому что нет сети (B8): файл не виноват, после возвращения сети — Play.
pub(crate) const QUEUE_STOPPED_NO_NETWORK_MESSAGE: &str = "No network connection; queue stopped";

/// Очередь встала, потому что сайт ограничил запросы (HTTP 429, B8).
pub(crate) const QUEUE_STOPPED_SITE_RATE_LIMITED_MESSAGE: &str =
    "Website limited requests - queue stopped";

/// Хвост сообщения, когда после пропусков открывать больше нечего.
const END_OF_QUEUE_SUFFIX: &str = "There is nothing else in the queue";

/// Как показать итог цепочки и каким текстом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaylistQueueNoticeDelivery {
    /// Информационная плашка в углу (исчезнет сама, 8 с).
    InfoToast(String),
    /// Временная плашка в углу (исчезнет сама, 5 с).
    TransientToast(String),
    /// Ошибка в центре: висит до ×, успешного открытия или нового открытия.
    MediaFailure(String),
    /// Ошибка в центре с кнопкой «Повторить», которая продолжает остановленную очередь
    /// (B9): то же, что Play на остановленном элементе.
    ResumableMediaFailure(String),
}

/// Выбирает вид показа и формулирует текст для одного итога цепочки.
pub(crate) fn playlist_queue_notice_delivery(
    notice: &AutomaticQueueNotice,
) -> PlaylistQueueNoticeDelivery {
    match notice {
        AutomaticQueueNotice::SkippedFailedItems { skipped } => {
            PlaylistQueueNoticeDelivery::InfoToast(skipped_items_sentence(skipped))
        }
        AutomaticQueueNotice::SkippedToEndOfQueue { skipped } => {
            PlaylistQueueNoticeDelivery::InfoToast(format!(
                "{}. {END_OF_QUEUE_SUFFIX}",
                skipped_items_sentence(skipped)
            ))
        }
        AutomaticQueueNotice::NothingInQueueOpened { .. } => {
            PlaylistQueueNoticeDelivery::MediaFailure(NOTHING_IN_QUEUE_OPENED_MESSAGE.to_owned())
        }
        AutomaticQueueNotice::StoppedOnFailedItem { failed_item } => {
            let text = match failed_item {
                Some(label) => format!("Queue stopped due to an error: {label}"),
                // Элемент мог исчезнуть из очереди раньше, чем controller узнал его имя.
                None => "Queue stopped due to a file error".to_owned(),
            };
            PlaylistQueueNoticeDelivery::TransientToast(text)
        }
        // Центр, а не toast: пользователь мог отойти, и причина остановки должна дождаться
        // его (висит до ×, нового открытия или успешной установки media).
        AutomaticQueueNotice::StoppedOpenBlocked { reason } => {
            PlaylistQueueNoticeDelivery::ResumableMediaFailure(
                match reason {
                    QueueBlockReason::NoNetwork => QUEUE_STOPPED_NO_NETWORK_MESSAGE,
                    QueueBlockReason::SiteRateLimited => QUEUE_STOPPED_SITE_RATE_LIMITED_MESSAGE,
                }
                .to_owned(),
            )
        }
    }
}

/// «Пропущено из-за ошибки 5 файлов: a.mkv, b.mp4, c.avi и ещё 2».
fn skipped_items_sentence(skipped: &SkippedItemsSummary) -> String {
    let total = skipped.total();
    let mut sentence = format!("Skipped {total} {} due to errors", files_noun(total));
    let names = skipped
        .named_items()
        .iter()
        .map(|label| label.as_str())
        .collect::<Vec<_>>();
    if !names.is_empty() {
        sentence.push_str(": ");
        sentence.push_str(&names.join(", "));
    }
    let unnamed_count = skipped.unnamed_count();
    if unnamed_count > 0 && !names.is_empty() {
        sentence.push_str(&format!(" and {unnamed_count} more"));
    }
    sentence
}

/// Английское существительное: единственное число только для ровно одного файла.
const fn files_noun(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

#[cfg(test)]
mod tests;
