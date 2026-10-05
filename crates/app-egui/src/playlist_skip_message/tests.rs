//! Формулировки и вид показа итогов автоматических пропусков.

use std::num::NonZeroUsize;

use super::{
    NOTHING_IN_QUEUE_OPENED_MESSAGE, PlaylistQueueNoticeDelivery, playlist_queue_notice_delivery,
};
use crate::media_open::SafeMediaLabel;
use crate::playlist_runtime::{AutomaticQueueNotice, SkippedItemsSummary};

fn summary(total: usize, names: &[&str]) -> SkippedItemsSummary {
    SkippedItemsSummary::from_test_parts(
        NonZeroUsize::new(total).expect("в сводке хотя бы один пропуск"),
        names,
    )
}

/// Пропуск с продолжением — информационная плашка с именем файла.
#[test]
fn single_skip_is_info_toast_with_file_name() {
    let notice = AutomaticQueueNotice::SkippedFailedItems {
        skipped: summary(1, &["b.mkv"]),
    };

    assert_eq!(
        playlist_queue_notice_delivery(&notice),
        PlaylistQueueNoticeDelivery::InfoToast("Пропущен из-за ошибки 1 файл: b.mkv".to_owned())
    );
}

/// До трёх имён, остальные — «и ещё K»; число согласовано с существительным.
#[test]
fn many_skips_name_three_and_count_the_rest() {
    let notice = AutomaticQueueNotice::SkippedFailedItems {
        skipped: summary(5, &["a.mkv", "b.mp4", "c.avi"]),
    };

    assert_eq!(
        playlist_queue_notice_delivery(&notice),
        PlaylistQueueNoticeDelivery::InfoToast(
            "Пропущено из-за ошибки 5 файлов: a.mkv, b.mp4, c.avi и ещё 2".to_owned()
        )
    );
}

/// Русское согласование числа: 1 файл, 2–4 файла, 5–20 файлов, 21 файл, 11–14 файлов.
#[test]
fn file_count_agrees_with_russian_grammar() {
    let cases = [
        (2, "Пропущено из-за ошибки 2 файла"),
        (4, "Пропущено из-за ошибки 4 файла"),
        (11, "Пропущено из-за ошибки 11 файлов"),
        (14, "Пропущено из-за ошибки 14 файлов"),
        (21, "Пропущен из-за ошибки 21 файл"),
        (22, "Пропущено из-за ошибки 22 файла"),
        (111, "Пропущено из-за ошибки 111 файлов"),
    ];
    for (total, expected) in cases {
        // Без имён: элементы исчезли из очереди, остаётся только число.
        let notice = AutomaticQueueNotice::SkippedFailedItems {
            skipped: summary(total, &[]),
        };
        assert_eq!(
            playlist_queue_notice_delivery(&notice),
            PlaylistQueueNoticeDelivery::InfoToast(expected.to_owned()),
            "{total}"
        );
    }
}

/// После пропусков открыть нечего, но что-то уже играло — плашка, а не ошибка в центре.
#[test]
fn end_of_queue_after_skips_is_info_toast() {
    let notice = AutomaticQueueNotice::SkippedToEndOfQueue {
        skipped: summary(2, &["b.mkv", "c.mkv"]),
    };

    assert_eq!(
        playlist_queue_notice_delivery(&notice),
        PlaylistQueueNoticeDelivery::InfoToast(
            "Пропущено из-за ошибки 2 файла: b.mkv, c.mkv. Дальше в очереди ничего нет".to_owned()
        )
    );
}

/// Не открылся ни один файл очереди — фатальная (закрываемая) ошибка в центре.
#[test]
fn nothing_opened_is_center_media_failure() {
    let notice = AutomaticQueueNotice::NothingInQueueOpened {
        skipped: summary(3, &["a.mkv", "b.mkv", "c.mkv"]),
    };

    assert_eq!(
        playlist_queue_notice_delivery(&notice),
        PlaylistQueueNoticeDelivery::MediaFailure(NOTHING_IN_QUEUE_OPENED_MESSAGE.to_owned())
    );
}

/// Явно выбранная политика «остановиться» — временная плашка с причиной.
#[test]
fn stop_policy_is_transient_toast_with_reason() {
    let named_stop = AutomaticQueueNotice::StoppedOnFailedItem {
        failed_item: Some(SafeMediaLabel::from_service_safe_label("b.mkv")),
    };
    let unnamed_stop = AutomaticQueueNotice::StoppedOnFailedItem { failed_item: None };

    assert_eq!(
        playlist_queue_notice_delivery(&named_stop),
        PlaylistQueueNoticeDelivery::TransientToast(
            "Очередь остановлена из-за ошибки: b.mkv".to_owned()
        )
    );
    assert_eq!(
        playlist_queue_notice_delivery(&unnamed_stop),
        PlaylistQueueNoticeDelivery::TransientToast(
            "Очередь остановлена из-за ошибки файла".to_owned()
        )
    );
}
