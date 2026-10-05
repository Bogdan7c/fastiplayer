//! Правила итогов цепочки пропусков на чистом владельце без очереди и player-а.

use playlist_core::AutomaticStopReason;

use super::{
    AutomaticQueueNotice, AutomaticSkipReport, MAX_NAMED_SKIPPED_ITEMS, SkipChainStart,
    SkippedItemsSummary,
};
use crate::media_open::SafeMediaLabel;
use crate::playlist_runtime::controller::AutomaticStopCause;

fn label(name: &str) -> Option<SafeMediaLabel> {
    Some(SafeMediaLabel::from_service_safe_label(name))
}

fn named(summary: &SkippedItemsSummary) -> Vec<&str> {
    summary
        .named_items()
        .iter()
        .map(SafeMediaLabel::as_str)
        .collect()
}

/// Пропуск и успешное продолжение дают одну сводку с именем, повторный drain пуст.
#[test]
fn installed_item_after_skip_reports_skipped_names_once() {
    let mut report = AutomaticSkipReport::default();
    report.begin_chain(SkipChainStart::AfterPlayback);
    report.record_failed_item(label("b.mkv"));

    report.finish_with_installed_item();

    let notices = report.drain_notices();
    let [AutomaticQueueNotice::SkippedFailedItems { skipped }] = notices.as_slice() else {
        panic!("ожидалась одна сводка пропусков, получено {notices:?}");
    };
    assert_eq!(skipped.total(), 1);
    assert_eq!(named(skipped), ["b.mkv"]);
    assert_eq!(skipped.unnamed_count(), 0);
    assert!(
        report.drain_notices().is_empty(),
        "итог отдаётся ровно один раз"
    );
}

/// Обычная смена файла без пропусков ничего не сообщает.
#[test]
fn chain_without_failures_reports_nothing() {
    let mut report = AutomaticSkipReport::default();
    report.begin_chain(SkipChainStart::AfterPlayback);
    report.finish_with_installed_item();
    report.begin_chain(SkipChainStart::AfterPlayback);
    report.finish_with_stop(
        AutomaticStopCause::Domain(AutomaticStopReason::EmptyQueue),
        3,
    );

    assert!(report.drain_notices().is_empty());
}

/// Имён не больше трёх, остальные считаются в «и ещё K»; безымянный элемент тоже считается.
#[test]
fn summary_names_first_three_and_counts_the_rest() {
    let mut report = AutomaticSkipReport::default();
    report.begin_chain(SkipChainStart::AfterPlayback);
    for name in ["a.mkv", "b.mkv", "c.mkv", "d.mkv"] {
        report.record_failed_item(label(name));
    }
    // Элемент исчез из очереди раньше, чем controller узнал его имя.
    report.record_failed_item(None);

    report.finish_with_installed_item();

    let notices = report.drain_notices();
    let [AutomaticQueueNotice::SkippedFailedItems { skipped }] = notices.as_slice() else {
        panic!("ожидалась одна сводка пропусков, получено {notices:?}");
    };
    assert_eq!(skipped.total(), 5);
    assert_eq!(named(skipped).len(), MAX_NAMED_SKIPPED_ITEMS);
    assert_eq!(named(skipped), ["a.mkv", "b.mkv", "c.mkv"]);
    assert_eq!(skipped.unnamed_count(), 2);
}

/// Политика `stop` называет элемент, на котором остановилась очередь.
#[test]
fn error_policy_stop_names_last_failed_item() {
    let mut report = AutomaticSkipReport::default();
    report.begin_chain(SkipChainStart::AfterPlayback);
    report.record_failed_item(label("broken.mkv"));

    report.finish_with_stop(AutomaticStopCause::ErrorPolicy, 3);

    assert_eq!(
        report.drain_notices(),
        [AutomaticQueueNotice::StoppedOnFailedItem {
            failed_item: label("broken.mkv"),
        }]
    );
}

/// «Ни один не открылся» — только если до цепочки ничего не играло и перебрана вся очередь.
#[test]
fn whole_queue_failure_is_distinguished_from_end_of_queue() {
    let cases = [
        // Рестарт: все три элемента очереди не открылись.
        (SkipChainStart::BeforeAnyPlayback, 3, 3, true),
        // Рестарт с середины очереди: начало очереди не пробовали — это не «ни один».
        (SkipChainStart::BeforeAnyPlayback, 2, 3, false),
        // Файл доиграл, а дальше всё битое — тоже не «ни один».
        (SkipChainStart::AfterPlayback, 3, 3, false),
    ];
    for (start, failed_count, queue_item_count, expect_nothing_opened) in cases {
        let mut report = AutomaticSkipReport::default();
        report.begin_chain(start);
        for index in 0..failed_count {
            report.record_failed_item(label(&format!("{index}.mkv")));
        }

        report.finish_with_stop(
            AutomaticStopCause::AllCandidatesFailed {
                attempted_count: failed_count,
            },
            queue_item_count,
        );

        let notices = report.drain_notices();
        let is_nothing_opened = matches!(
            notices.as_slice(),
            [AutomaticQueueNotice::NothingInQueueOpened { .. }]
        );
        let is_end_of_queue = matches!(
            notices.as_slice(),
            [AutomaticQueueNotice::SkippedToEndOfQueue { .. }]
        );
        assert_eq!(
            is_nothing_opened, expect_nothing_opened,
            "{start:?}/{failed_count}"
        );
        assert_eq!(
            is_end_of_queue, !expect_nothing_opened,
            "{start:?}/{failed_count}"
        );
    }
}

/// Остановку пользователем или изменение очереди не превращаем в сообщение об ошибке.
#[test]
fn user_or_structural_stop_discards_chain_silently() {
    for cause in [
        AutomaticStopCause::ManualTraversalCancelled,
        AutomaticStopCause::StructuralInvalidation,
        AutomaticStopCause::DeferredCancelled,
    ] {
        let mut report = AutomaticSkipReport::default();
        report.begin_chain(SkipChainStart::AfterPlayback);
        report.record_failed_item(label("b.mkv"));

        report.finish_with_stop(cause, 3);

        assert!(report.drain_notices().is_empty(), "{cause:?}");
    }
}

/// Явный выбор пользователя прерывает цепочку: последующий успех ничего не сообщает.
#[test]
fn discarded_chain_does_not_leak_into_next_install() {
    let mut report = AutomaticSkipReport::default();
    report.begin_chain(SkipChainStart::AfterPlayback);
    report.record_failed_item(label("b.mkv"));

    report.discard_chain();
    report.finish_with_installed_item();

    assert!(report.drain_notices().is_empty());
}

/// Новая цепочка не наследует пропуски прежней незаконченной.
#[test]
fn new_chain_starts_from_zero() {
    let mut report = AutomaticSkipReport::default();
    report.begin_chain(SkipChainStart::AfterPlayback);
    report.record_failed_item(label("old.mkv"));

    report.begin_chain(SkipChainStart::AfterPlayback);
    report.record_failed_item(label("new.mkv"));
    report.finish_with_installed_item();

    let notices = report.drain_notices();
    let [AutomaticQueueNotice::SkippedFailedItems { skipped }] = notices.as_slice() else {
        panic!("ожидалась одна сводка пропусков, получено {notices:?}");
    };
    assert_eq!(named(skipped), ["new.mkv"]);
    assert_eq!(skipped.total(), 1);
}
