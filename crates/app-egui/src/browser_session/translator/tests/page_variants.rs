//! Варианты страницы (14a) в переходнике: `variant_menu_changed` и `page_navigation` уходят
//! главному циклу уведомлением в порядке провода, писателей потока не касаются; подсказка
//! субтитров 1.7 идёт тем же путём.

use browser_bridge_protocol::session::{
    NextEpisodeAvailability, SubtitleChoice, VariantKind, VariantMenu,
};

use super::*;
use crate::browser_session::page_variants::PageVariantUpdate;

fn quality_menu() -> VariantMenu {
    VariantMenu {
        kind: VariantKind::Quality,
        options: Vec::new(),
    }
}

#[test]
fn variant_words_reach_the_main_loop_in_wire_order_without_touching_the_stream() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            message(
                1,
                PageSessionMessage::VariantMenuChanged {
                    menu: quality_menu(),
                },
            ),
            message(
                1,
                PageSessionMessage::PageNavigation {
                    next_episode: NextEpisodeAvailability::Available,
                },
            ),
        ],
    );
    let variant_words: Vec<Seen> = seen(&notices)
        .into_iter()
        .filter(|word| matches!(word, Seen::PageVariants(..)))
        .collect();
    assert_eq!(
        variant_words,
        vec![
            Seen::PageVariants(1, PageVariantUpdate::Menu(quality_menu())),
            Seen::PageVariants(
                1,
                PageVariantUpdate::Navigation(NextEpisodeAvailability::Available)
            ),
        ]
    );
    let counters = translator.counters();
    assert_eq!(counters.page_variant_words, 2);
    assert_eq!(counters.ignored_messages, 0);
    // Писатель потока получил ровно то же, что без слов о вариантах: они не его дело.
    let (mut plain, plain_log, _plain_failure) = fake_translator();
    let _plain_notices = run(&mut plain, [started(1, 1)]);
    assert_eq!(log.calls(), plain_log.calls());
}

/// Подсказка субтитров 1.7 (14a P2b) идёт тем же путём, что слова о вариантах: одно
/// уведомление в порядке провода, писателей потока не трогает.
#[test]
fn the_subtitle_preference_reaches_the_main_loop_as_a_page_variant_word() {
    let (mut translator, _log, _failure) = fake_translator();
    let choice = SubtitleChoice::Off {};
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            message(1, PageSessionMessage::SubtitlePreference { choice }),
        ],
    );
    let variant_words: Vec<Seen> = seen(&notices)
        .into_iter()
        .filter(|word| matches!(word, Seen::PageVariants(..)))
        .collect();
    assert_eq!(
        variant_words,
        vec![Seen::PageVariants(
            1,
            PageVariantUpdate::SubtitlePreference(choice)
        )]
    );
    assert_eq!(translator.counters().page_variant_words, 1);
    assert_eq!(translator.counters().ignored_messages, 0);
}
