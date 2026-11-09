//! Виджеты вариантов страницы против правил окна (14a P2b): выбор из меню «♪» по старой
//! ревизии отвергается, список качеств страницы попадает в меню кнопки качества, а во время
//! рекламы пункты обоих меню помечены заблокированными.

use std::sync::Arc;

use super::quality_menu::test_support::snapshot_with_frame;
use super::*;
use crate::ad_plate::{AdActionProgress, AdPlate, AdPodLabel, AdSkipOffer};
use crate::page_quality::{PageQualityFacts, PageViewOffer};
use crate::page_variants::{
    NextEpisodeOffer, PageVariantAvailability, PageVariantFacts, PageVariantIndex, PageVariantKind,
    PageVariantMenuRevision, PageVariantMenuView, PageVariantSelection,
};
use crate::state::{AudioTrackMenuTarget, AudioTrackMenuView};
use crate::ui::player_controls::QualityControlAction;

fn menu(kind: PageVariantKind, revision: u64, labels: &[&str]) -> PageVariantMenuView {
    PageVariantMenuView::new(
        kind,
        PageVariantMenuRevision::new(revision),
        labels.iter().map(|label| Arc::from(*label)).collect(),
        Some(PageVariantIndex::new(0)),
    )
}

fn facts(audio_revision: u64) -> PageVariantFacts {
    PageVariantFacts {
        audio: Some(menu(
            PageVariantKind::Audio,
            audio_revision,
            &["Original", "Dub"],
        )),
        quality: Some(menu(PageVariantKind::Quality, 4, &["1080p", "720p"])),
        next_episode: NextEpisodeOffer::Offered,
    }
}

fn ad_plate() -> AdPlate {
    AdPlate {
        pod: AdPodLabel::Unknown,
        offer: AdSkipOffer::Nothing,
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

/// Окно браузерного режима, где есть что показать в кнопке качества.
fn browser_window() -> WindowMode {
    let mut browser = WindowMode::browser_session();
    browser.note_page_quality(PageQualityFacts {
        bitrate: None,
        page_view: PageViewOffer::Available,
    });
    browser
}

#[test]
fn a_dub_picked_from_a_menu_that_changed_since_is_refused_and_nothing_is_queued() {
    let mut browser = browser_window();
    browser.note_page_variants(facts(3));
    let shown = AudioTrackMenuView::for_page_dubs_test(
        &player_core::PlayerSnapshot::empty(),
        browser
            .page_variants
            .menu(PageVariantKind::Audio)
            .expect("audio menu"),
        browser.page_variant_availability(),
    );
    let AudioTrackMenuTarget::PageDub(stale_pick) = shown.tracks()[1].target else {
        panic!("the menu lists page dubs");
    };
    // Страница сменила меню, пока зритель держал «♪» открытым.
    browser.note_page_variants(facts(5));

    assert_eq!(
        browser.select_page_variant(stale_pick),
        PageVariantSelectOutcome::StaleMenu
    );
    assert!(drained_intents(&mut browser).is_empty());
}

#[test]
fn the_quality_menu_lists_the_page_qualities_with_the_mark_and_routes_a_pick() {
    let mut browser = browser_window();
    browser.note_page_variants(facts(3));
    let view = browser.quality_menu_view(&snapshot_with_frame((1280, 720)));
    let choices = view.page_qualities().expect("the page told its qualities");
    let rows: Vec<(&str, PageVariantSelection)> = choices
        .menu()
        .options()
        .map(|option| (option.label, option.selection))
        .collect();
    assert_eq!(
        rows,
        [
            ("1080p", PageVariantSelection::Selected),
            ("720p", PageVariantSelection::Available)
        ]
    );
    assert_eq!(choices.availability(), PageVariantAvailability::Selectable);
    // Подпись кнопки — по картинке на экране (13c), а не по пункту страницы.
    assert_eq!(view.tier_label().as_deref(), Some("720p"));

    let pick = choices
        .menu()
        .choice(PageVariantIndex::new(1))
        .expect("720p");
    browser.apply_quality_control_action(QualityControlAction::SelectPageQuality(pick));
    assert_eq!(
        drained_intents(&mut browser),
        vec![ViewerTransportIntent::SelectPageVariant(pick)]
    );
}

#[test]
fn without_a_page_quality_menu_there_is_no_list() {
    let mut browser = browser_window();
    browser.note_page_variants(PageVariantFacts {
        quality: None,
        ..facts(3)
    });
    let view = browser.quality_menu_view(&snapshot_with_frame((1280, 720)));
    assert!(view.is_shown());
    assert!(view.page_qualities().is_none());
}

#[test]
fn during_an_ad_both_menus_mark_their_page_rows_blocked() {
    let mut browser = browser_window();
    browser.note_page_variants(facts(3));
    browser.note_ad_plate(Some(ad_plate()));
    assert_eq!(
        browser.page_variant_availability(),
        PageVariantAvailability::BlockedDuringAd
    );
    let view = browser.quality_menu_view(&snapshot_with_frame((1280, 720)));
    assert_eq!(
        view.page_qualities().map(|choices| choices.availability()),
        Some(PageVariantAvailability::BlockedDuringAd)
    );
    browser.note_ad_plate(None);
    assert_eq!(
        browser.page_variant_availability(),
        PageVariantAvailability::Selectable
    );
}
