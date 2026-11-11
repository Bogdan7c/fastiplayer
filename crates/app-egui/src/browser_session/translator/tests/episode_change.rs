//! Слова протокола 1.9 о смене серии на той же странице (`awaiting_next_episode`,
//! `page_title_changed`) в переходнике до их обработчика.
//!
//! 16a P-A1: пока они считаются пропущенными и ничего не меняют — ни уведомлений главному
//! циклу, ни вызовов писателя потока. Обработчик P-A1 должен заменить этот тест своим
//! (уведомление главному циклу), а не просто удалить его.

use browser_bridge_protocol::PageTitle;
use browser_bridge_protocol::session::NextEpisodePatience;

use super::*;

#[test]
fn episode_change_words_are_counted_as_ignored_and_change_nothing_yet() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            message(
                1,
                PageSessionMessage::AwaitingNextEpisode {
                    patience_ms: NextEpisodePatience::from_millis(60_000),
                },
            ),
            message(
                1,
                PageSessionMessage::PageTitleChanged {
                    title: PageTitle::new("Серія 4").unwrap(),
                },
            ),
        ],
    );
    // Уведомления — ровно те же, что от одного начала поколения.
    let (mut plain, plain_log, _plain_failure) = fake_translator();
    let plain_notices = run(&mut plain, [started(1, 1)]);
    assert_eq!(seen(&notices), seen(&plain_notices));
    // Оба слова учтены как пропущенные, писатель потока их не видел.
    assert_eq!(translator.counters().ignored_messages, 2);
    assert_eq!(log.calls(), plain_log.calls());
}
