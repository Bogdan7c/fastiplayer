//! Варианты страницы в режиме окна: выбор по своему последнему слову, отказ в рекламе,
//! «следующая серия» и маршрут Next (плашка рекламы → следующая серия → как раньше).

use std::sync::Arc;

use super::*;
use crate::ad_plate::{AdActionProgress, AdPlate, AdPodLabel, AdSkipOffer};
use crate::page_variants::{
    NextEpisodeOffer, PageVariantIndex, PageVariantMenuRevision, PageVariantMenuView,
};

fn audio_menu(revision: u64) -> PageVariantMenuView {
    PageVariantMenuView::new(
        PageVariantKind::Audio,
        PageVariantMenuRevision::new(revision),
        vec![Arc::from("Original"), Arc::from("Dub")],
        Some(PageVariantIndex::new(0)),
    )
}

fn facts(next_episode: NextEpisodeOffer) -> PageVariantFacts {
    PageVariantFacts {
        audio: Some(audio_menu(3)),
        quality: None,
        subtitles: None,
        next_episode,
    }
}

fn ad_plate(offer: AdSkipOffer) -> AdPlate {
    AdPlate {
        pod: AdPodLabel::Unknown,
        offer,
        progress: AdActionProgress::Idle,
        locks_transport: true,
    }
}

fn drained_intents(mode: &mut WindowMode) -> Vec<ViewerTransportIntent> {
    match &mut mode.transport {
        TransportRoute::PageLed(outbox) => outbox.take(),
        TransportRoute::LocalPlayer => Vec::new(),
    }
}

fn dub_choice(revision: u64) -> PageVariantChoice {
    PageVariantChoice {
        kind: PageVariantKind::Audio,
        revision: PageVariantMenuRevision::new(revision),
        index: PageVariantIndex::new(1),
    }
}

#[test]
fn only_the_page_led_window_keeps_the_variant_facts() {
    let mut standard = WindowMode::standard();
    assert!(!standard.note_page_variants(facts(NextEpisodeOffer::Offered)));
    assert_eq!(standard.page_variants, PageVariantFacts::default());

    let mut browser = WindowMode::browser_session();
    assert!(browser.note_page_variants(facts(NextEpisodeOffer::Offered)));
    assert!(!browser.note_page_variants(facts(NextEpisodeOffer::Offered)));
}

#[test]
fn a_choice_against_the_current_menu_reaches_the_outbox() {
    let mut browser = WindowMode::browser_session();
    browser.note_page_variants(facts(NextEpisodeOffer::PageSaysUnavailable));
    assert_eq!(
        browser.select_page_variant(dub_choice(3)),
        PageVariantSelectOutcome::Queued
    );
    assert_eq!(
        drained_intents(&mut browser),
        vec![ViewerTransportIntent::SelectPageVariant(dub_choice(3))]
    );
}

#[test]
fn a_stale_absent_or_out_of_range_choice_stays_in_the_window() {
    let mut browser = WindowMode::browser_session();
    assert_eq!(
        browser.select_page_variant(dub_choice(3)),
        PageVariantSelectOutcome::NoMenu
    );
    browser.note_page_variants(facts(NextEpisodeOffer::PageSaysUnavailable));
    assert_eq!(
        browser.select_page_variant(dub_choice(2)),
        PageVariantSelectOutcome::StaleMenu
    );
    let beyond = PageVariantChoice {
        index: PageVariantIndex::new(2),
        ..dub_choice(3)
    };
    assert_eq!(
        browser.select_page_variant(beyond),
        PageVariantSelectOutcome::NoSuchOption
    );
    assert_eq!(drained_intents(&mut browser), vec![]);

    let mut standard = WindowMode::standard();
    assert_eq!(
        standard.select_page_variant(dub_choice(3)),
        PageVariantSelectOutcome::NotPageLed
    );
}

#[test]
fn during_an_ad_the_selection_is_refused_and_nothing_is_queued() {
    let mut browser = WindowMode::browser_session();
    browser.note_page_variants(facts(NextEpisodeOffer::Offered));
    browser.note_ad_plate(Some(ad_plate(AdSkipOffer::Nothing)));
    assert_eq!(
        browser.select_page_variant(dub_choice(3)),
        PageVariantSelectOutcome::RefusedDuringAd
    );
    assert_eq!(drained_intents(&mut browser), vec![]);
}

#[test]
fn next_episode_is_offered_only_when_the_page_says_so_and_no_ad_is_on_screen() {
    let mut browser = WindowMode::browser_session();
    assert!(!browser.next_episode_offered(), "nothing said yet");
    for refused in [
        NextEpisodeOffer::PageSaysUnavailable,
        NextEpisodeOffer::ExtensionTooOld,
        NextEpisodeOffer::NotAttached,
        NextEpisodeOffer::PageAway,
    ] {
        browser.note_page_variants(facts(refused));
        assert!(!browser.next_episode_offered(), "{refused:?}");
        assert_eq!(
            browser.request_next_episode(),
            NextEpisodeWindowOutcome::NotOffered
        );
    }
    browser.note_page_variants(facts(NextEpisodeOffer::Offered));
    assert!(browser.next_episode_offered());
    browser.note_ad_plate(Some(ad_plate(AdSkipOffer::Nothing)));
    assert!(!browser.next_episode_offered(), "the ad comes first");
    assert_eq!(drained_intents(&mut browser), vec![]);

    let standard = WindowMode::standard();
    assert!(!standard.next_episode_offered());
}

/// «Следующее» зрителя: реклама с кнопкой → пропуск, отсчёт рекламы → ничего (серия не
/// предлагается), вне рекламы → следующая серия, без серии → ничего (как раньше).
#[test]
fn next_presses_the_ad_offer_first_then_asks_for_the_next_episode() {
    let mut browser = WindowMode::browser_session();
    browser.note_page_variants(facts(NextEpisodeOffer::Offered));

    browser.note_ad_plate(Some(ad_plate(AdSkipOffer::Skip)));
    assert_eq!(browser.activate_next(), NextActivation::AdOfferRequested);
    assert_eq!(
        drained_intents(&mut browser),
        vec![ViewerTransportIntent::SkipAd],
        "N during an ad skips the ad, not the episode"
    );

    browser.note_ad_plate(Some(ad_plate(AdSkipOffer::Countdown { seconds: 5 })));
    assert_eq!(browser.activate_next(), NextActivation::NothingToDo);
    assert_eq!(drained_intents(&mut browser), vec![]);

    browser.note_ad_plate(None);
    assert_eq!(
        browser.activate_next(),
        NextActivation::NextEpisodeRequested
    );
    assert_eq!(
        drained_intents(&mut browser),
        vec![ViewerTransportIntent::NextEpisode]
    );

    browser.note_page_variants(facts(NextEpisodeOffer::PageSaysUnavailable));
    assert_eq!(browser.activate_next(), NextActivation::NothingToDo);
    assert_eq!(drained_intents(&mut browser), vec![]);
}

// ---- тосты исхода выбора (15c2) ------------------------------------------------------

/// Тексты видимых плашек центра (новые сверху).
fn visible_toast_texts(center: &mut NotificationCenter, now: Instant) -> Vec<String> {
    center
        .frame(
            crate::state::notifications::OpenProgress::Idle,
            crate::ui::animation::UiMotion::Standard,
            now,
        )
        .toasts
        .iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

fn show_all(center: &mut NotificationCenter, notices: &[PageVariantNotice], now: Instant) {
    for notice in notices {
        show_page_variant_toast(center, notice, now);
    }
}

#[test]
fn audio_switching_is_replaced_by_the_refusal_reason_in_one_toast() {
    use crate::page_variants::{PageVariantRefusal, PageVariantSubject};
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    show_all(
        &mut center,
        &[PageVariantNotice::Switching {
            kind: PageVariantKind::Audio,
        }],
        now,
    );
    assert_eq!(
        visible_toast_texts(&mut center, now),
        ["Switching audio on the page\u{2026}"]
    );
    show_all(
        &mut center,
        &[PageVariantNotice::Refused {
            subject: PageVariantSubject::Menu(PageVariantKind::Audio),
            reason: PageVariantRefusal::NoSuchEpisode,
        }],
        now,
    );
    assert_eq!(
        visible_toast_texts(&mut center, now),
        ["This dub has no such episode \u{2014} pick an earlier episode on the page"]
    );
}

#[test]
fn quality_switching_is_replaced_by_done_and_does_not_touch_the_audio_toast() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    show_all(
        &mut center,
        &[
            PageVariantNotice::Switching {
                kind: PageVariantKind::Audio,
            },
            PageVariantNotice::Switching {
                kind: PageVariantKind::Quality,
            },
            PageVariantNotice::Selected {
                kind: PageVariantKind::Quality,
                label: Arc::from("720p"),
            },
        ],
        now,
    );
    assert_eq!(
        visible_toast_texts(&mut center, now),
        ["Quality: 720p", "Switching audio on the page\u{2026}"]
    );
}

#[test]
fn the_next_episode_refusal_replaces_its_own_toast_and_subtitles_show_none() {
    use crate::page_variants::{PageVariantRefusal, PageVariantSubject};
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    show_all(
        &mut center,
        &[
            PageVariantNotice::NextEpisodeRequested,
            PageVariantNotice::Refused {
                subject: PageVariantSubject::NextEpisode,
                reason: PageVariantRefusal::Failed,
            },
            PageVariantNotice::Selected {
                kind: PageVariantKind::Subtitles,
                label: Arc::from("uk"),
            },
        ],
        now,
    );
    assert_eq!(
        visible_toast_texts(&mut center, now),
        ["Could not open the next episode on the page"]
    );
}
