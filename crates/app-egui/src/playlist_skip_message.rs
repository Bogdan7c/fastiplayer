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

use crate::playlist_runtime::{AutomaticQueueNotice, SkippedItemsSummary};

/// Текст, когда не открылся ни один элемент очереди.
pub(crate) const NOTHING_IN_QUEUE_OPENED_MESSAGE: &str = "Ни один файл очереди не удалось открыть";

/// Хвост сообщения, когда после пропусков открывать больше нечего.
const END_OF_QUEUE_SUFFIX: &str = "Дальше в очереди ничего нет";

/// Как показать итог цепочки и каким текстом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaylistQueueNoticeDelivery {
    /// Информационная плашка в углу (исчезнет сама, 8 с).
    InfoToast(String),
    /// Временная плашка в углу (исчезнет сама, 5 с).
    TransientToast(String),
    /// Ошибка в центре: висит до ×, успешного открытия или нового открытия.
    MediaFailure(String),
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
                Some(label) => format!("Очередь остановлена из-за ошибки: {label}"),
                // Элемент мог исчезнуть из очереди раньше, чем controller узнал его имя.
                None => "Очередь остановлена из-за ошибки файла".to_owned(),
            };
            PlaylistQueueNoticeDelivery::TransientToast(text)
        }
    }
}

/// «Пропущено из-за ошибки 5 файлов: a.mkv, b.mp4, c.avi и ещё 2».
fn skipped_items_sentence(skipped: &SkippedItemsSummary) -> String {
    let total = skipped.total();
    let mut sentence = format!(
        "{} из-за ошибки {total} {}",
        skipped_verb(total),
        files_noun(total)
    );
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
        sentence.push_str(&format!(" и ещё {unnamed_count}"));
    }
    sentence
}

/// Согласование глагола с числом: «Пропущен 1 файл», «Пропущено 2 файла».
const fn skipped_verb(count: usize) -> &'static str {
    match russian_plural_form(count) {
        RussianPluralForm::One => "Пропущен",
        RussianPluralForm::Few | RussianPluralForm::Many => "Пропущено",
    }
}

/// «файл» / «файла» / «файлов» по правилам русского языка.
const fn files_noun(count: usize) -> &'static str {
    match russian_plural_form(count) {
        RussianPluralForm::One => "файл",
        RussianPluralForm::Few => "файла",
        RussianPluralForm::Many => "файлов",
    }
}

/// Три формы русского существительного после числа.
///
/// Общая для текстов уведомлений (её же использует `startup_arguments_message`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RussianPluralForm {
    /// 1, 21, 101 — «файл».
    One,
    /// 2–4, 22–24 — «файла».
    Few,
    /// 0, 5–20, 25–30, 111–114 — «файлов».
    Many,
}

/// Числа на 11–14 всегда во множественной форме, иначе решает последняя цифра.
pub(crate) const fn russian_plural_form(count: usize) -> RussianPluralForm {
    let last_two_digits = count % 100;
    if last_two_digits >= 11 && last_two_digits <= 14 {
        return RussianPluralForm::Many;
    }
    match count % 10 {
        1 => RussianPluralForm::One,
        2..=4 => RussianPluralForm::Few,
        _ => RussianPluralForm::Many,
    }
}

#[cfg(test)]
mod tests;
