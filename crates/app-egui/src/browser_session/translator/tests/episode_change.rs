//! Слова протокола 1.9 о смене серии на той же странице (`awaiting_next_episode`,
//! `page_title_changed`) в переходнике (16a): уходят главному циклу уведомлением в порядке
//! провода, писателей потока не касаются, чужому (старому) экземпляру не достаются.

use browser_bridge_protocol::PageTitle;
use browser_bridge_protocol::session::NextEpisodePatience;

use super::*;
use crate::browser_session::intake_notice::PageEpisodeWord;

fn awaiting(instance: u64) -> SessionInput {
    message(
        instance,
        PageSessionMessage::AwaitingNextEpisode {
            patience_ms: NextEpisodePatience::from_millis(60_000),
        },
    )
}

fn retitled(instance: u64, title: &str) -> SessionInput {
    message(
        instance,
        PageSessionMessage::PageTitleChanged {
            title: PageTitle::new(title).expect("valid title"),
        },
    )
}

/// Только слова о смене серии из всех уведомлений прогона.
fn episode_words(notices: &[IntakeNotice]) -> Vec<Seen> {
    seen(notices)
        .into_iter()
        .filter(|word| matches!(word, Seen::EpisodeChange(..)))
        .collect()
}

#[test]
fn episode_change_words_reach_the_main_loop_in_wire_order_without_touching_the_stream() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            awaiting(1),
            retitled(1, "Серія 4"),
            // Пустой заголовок допустим (страница без `<title>`).
            retitled(1, ""),
        ],
    );
    assert_eq!(
        episode_words(&notices),
        vec![
            Seen::EpisodeChange(
                1,
                PageEpisodeWord::AwaitingNextEpisode(NextEpisodePatience::from_millis(60_000))
            ),
            Seen::EpisodeChange(
                1,
                PageEpisodeWord::TitleChanged(PageTitle::new("Серія 4").expect("valid title"))
            ),
            Seen::EpisodeChange(
                1,
                PageEpisodeWord::TitleChanged(PageTitle::new("").expect("valid title"))
            ),
        ]
    );
    let counters = translator.counters();
    assert_eq!(counters.episode_change_words, 3);
    assert_eq!(counters.ignored_messages, 0, "nothing is ignored any more");
    // Писатель потока получил ровно то же, что без этих слов: они не его дело.
    let (mut plain, plain_log, _plain_failure) = fake_translator();
    let plain_notices = run(&mut plain, [started(1, 1)]);
    assert_eq!(log.calls(), plain_log.calls());
    // Кроме самих слов, уведомления те же, что от одного начала поколения.
    let other_notices: Vec<Seen> = seen(&notices)
        .into_iter()
        .filter(|word| !matches!(word, Seen::EpisodeChange(..)))
        .collect();
    assert_eq!(other_notices, seen(&plain_notices));
}

#[test]
fn episode_change_words_of_a_replaced_instance_are_not_forwarded() {
    let (mut translator, _log, _failure) = fake_translator();
    // Экземпляр 2 заменил экземпляр 1: запоздалые слова старой вкладки не доходят.
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            started(2, 1),
            awaiting(1),
            retitled(1, "Old"),
        ],
    );
    assert_eq!(episode_words(&notices), Vec::<Seen>::new());
    assert_eq!(translator.counters().episode_change_words, 0);
}
