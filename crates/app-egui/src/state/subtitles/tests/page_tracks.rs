//! Дорожки страницы браузера в владельце субтитров (мост, 13b1): склад → меню → выбор →
//! разбор в фоне → реплики на кадре. Идёт настоящим путём: `attach_page_tracks`, фоновая
//! задача, приём результата, `placed_overlay_for_frame`.

use fastiplayer_config::SubtitlesConfig;

use super::ass_gpu_support::{full_window_picture, window_frame};
use super::embedded_support::GENERATION;
use super::page_support::*;
use super::track_selection::settle;
use super::web_support::{click_menu_row, menu_texts};
use super::*;
use crate::page_subtitles::{PageSubtitleDisplay, PageTracksUpdate};
use crate::state::subtitles::menu_view::{
    SubtitleFileProblem, SubtitleTrackAvailability, SubtitleUnavailableReason,
};
use crate::state::subtitles::owner_page::PageAdPresence;

const SHOWING: PageSubtitleDisplay = PageSubtitleDisplay::ShowingOnPage;
const NOT_SHOWING: PageSubtitleDisplay = PageSubtitleDisplay::NotShowingOnPage;

fn rows(owner: &SubtitleOwner) -> Vec<String> {
    menu_texts(owner)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

fn ru_preferred_enabled() -> SubtitlesConfig {
    SubtitlesConfig {
        enabled: true,
        preferred_languages: "ru".to_owned(),
        ..SubtitlesConfig::default()
    }
}

#[test]
fn a_track_the_page_was_showing_appears_in_the_menu_and_is_shown_although_subtitles_are_off() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    let view = shelf.store(page_document(
        1,
        Some("en"),
        Some("English (auto-generated)"),
        SHOWING,
        vtt_with("Hello from the page").as_bytes(),
    ));

    assert_eq!(offer(&mut owner, &view), PageTracksUpdate::Changed);

    assert_eq!(rows(&owner), ["English (auto-generated)"]);
    assert_eq!(
        shown_row(&owner).as_deref(),
        Some("English (auto-generated)")
    );
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    assert_eq!(displayed_texts_at(&owner, 2_000), ["Hello from the page"]);
    // Страница включила субтитры только этому медиа: конфиг и запомненное «вкл» не трогаются.
    assert_eq!(owner.take_enabled_preference_change(), None);
    assert_eq!(owner.default_visibility, SubtitleVisibility::Hidden);
    // Тот же склад второй раз — ничего нового.
    assert_eq!(owner.attach_page_tracks(&view), PageTracksUpdate::Unchanged);
}

#[test]
fn tracks_the_page_was_not_showing_follow_the_ordinary_language_auto_select() {
    let media = instance(1);
    let mut shelf = TestShelf::new();
    shelf.store(page_document(
        1,
        Some("en"),
        None,
        NOT_SHOWING,
        vtt_with("en").as_bytes(),
    ));
    let view = shelf.store(page_document(
        2,
        Some("ru"),
        None,
        NOT_SHOWING,
        vtt_with("ru").as_bytes(),
    ));

    let mut enabled = browser_owner(&ru_preferred_enabled(), media);
    offer(&mut enabled, &view);
    assert_eq!(rows(&enabled), ["English", "Russian"]);
    assert_eq!(shown_row(&enabled).as_deref(), Some("Russian"));
    assert_eq!(displayed_texts_at(&enabled, 2_000), ["ru"]);

    // Субтитры выключены и страница ничего не показывала — показывать нечего.
    let mut disabled = browser_owner(&SubtitlesConfig::default(), media);
    offer(&mut disabled, &view);
    assert_eq!(rows(&disabled).len(), 2);
    assert_eq!(shown_row(&disabled), None);
}

#[test]
fn the_latest_showing_track_wins_over_an_earlier_one() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            None,
            SHOWING,
            vtt_with("en").as_bytes(),
        )),
    );
    offer(
        &mut owner,
        &shelf.store(page_document(
            2,
            Some("de"),
            None,
            SHOWING,
            vtt_with("de").as_bytes(),
        )),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("German"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["de"]);
}

#[test]
fn the_viewer_choice_is_never_overridden_by_a_later_showing_document() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    shelf.store(page_document(
        1,
        Some("en"),
        None,
        SHOWING,
        vtt_with("en").as_bytes(),
    ));
    offer(
        &mut owner,
        &shelf.store(page_document(
            2,
            Some("ru"),
            None,
            NOT_SHOWING,
            vtt_with("ru").as_bytes(),
        )),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));

    click_menu_row(&mut owner, "Russian");
    settle(&mut owner);
    // Страница прислала новую дорожку с `showing` и заменила английскую тоже с `showing`.
    offer(
        &mut owner,
        &shelf.store(page_document(
            3,
            Some("de"),
            None,
            SHOWING,
            vtt_with("de").as_bytes(),
        )),
    );
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            None,
            SHOWING,
            vtt_with("en 2").as_bytes(),
        )),
    );

    assert_eq!(rows(&owner), ["English", "Russian", "German"]);
    assert_eq!(
        shown_row(&owner).as_deref(),
        Some("Russian"),
        "the viewer is in charge"
    );
    assert_eq!(displayed_texts_at(&owner, 2_000), ["ru"]);
}

#[test]
fn subtitles_the_viewer_turned_off_stay_off_when_the_page_shows_a_track_again() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            None,
            SHOWING,
            vtt_with("en").as_bytes(),
        )),
    );
    assert!(shown_row(&owner).is_some());

    owner.set_visibility(SubtitleVisibility::Hidden);
    offer(
        &mut owner,
        &shelf.store(page_document(
            2,
            Some("de"),
            None,
            SHOWING,
            vtt_with("de").as_bytes(),
        )),
    );
    assert_eq!(shown_row(&owner), None);
    assert!(displayed_texts_at(&owner, 2_000).is_empty());
}

#[test]
fn a_garbage_document_fails_only_its_own_track() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    shelf.store(page_document(
        1,
        Some("en"),
        Some("Broken"),
        SHOWING,
        b"\x00\x01 not subtitles",
    ));
    let view = shelf.store(page_document(
        2,
        Some("ru"),
        None,
        NOT_SHOWING,
        vtt_with("ru").as_bytes(),
    ));
    offer(&mut owner, &view);

    let menu = owner.menu_view();
    assert_eq!(
        menu.tracks[0].availability,
        SubtitleTrackAvailability::Unavailable(SubtitleUnavailableReason::LoadFailed(
            SubtitleFileProblem::CannotParse
        ))
    );
    assert_eq!(
        menu.tracks[1].availability,
        SubtitleTrackAvailability::Available
    );
    assert!(displayed_texts_at(&owner, 2_000).is_empty());

    click_menu_row(&mut owner, "Russian");
    settle(&mut owner);
    assert_eq!(displayed_texts_at(&owner, 2_000), ["ru"]);
}

#[test]
fn a_replaced_document_keeps_its_menu_row_and_is_parsed_again() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            4,
            Some("en"),
            None,
            SHOWING,
            vtt_with("old words").as_bytes(),
        )),
    );
    let selected_before = owner.menu_view().tracks[0].id;
    assert_eq!(displayed_texts_at(&owner, 2_000), ["old words"]);

    offer(
        &mut owner,
        &shelf.store(page_document(
            4,
            Some("en"),
            None,
            SHOWING,
            vtt_with("new words").as_bytes(),
        )),
    );

    let menu = owner.menu_view();
    assert_eq!(menu.tracks.len(), 1);
    assert_eq!(menu.tracks[0].id, selected_before);
    assert_eq!(displayed_texts_at(&owner, 2_000), ["new words"]);
}

#[test]
fn a_new_media_of_the_same_session_gets_every_page_track_and_the_showing_choice_again() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    shelf.store(page_document(
        1,
        Some("en"),
        None,
        NOT_SHOWING,
        vtt_with("en").as_bytes(),
    ));
    let view = shelf.store(page_document(
        2,
        Some("ru"),
        None,
        SHOWING,
        vtt_with("ru").as_bytes(),
    ));
    offer(&mut owner, &view);

    // Новое поколение потока = новое медиа: его состояние начинается с нуля.
    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    assert!(rows(&owner).is_empty());
    assert_eq!(offer(&mut owner, &view), PageTracksUpdate::Changed);

    assert_eq!(rows(&owner), ["English", "Russian"]);
    assert_eq!(shown_row(&owner).as_deref(), Some("Russian"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["ru"]);
}

#[test]
fn media_of_the_old_session_ignores_the_shelf_of_a_new_session_and_the_new_media_takes_it() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut old_shelf = TestShelf::new();
    offer(
        &mut owner,
        &old_shelf.store(page_document(
            1,
            Some("en"),
            None,
            SHOWING,
            vtt_with("old tab").as_bytes(),
        )),
    );
    let mut new_shelf = old_shelf.of_next_session();
    let new_view = new_shelf.store(page_document(
        1,
        Some("ja"),
        None,
        SHOWING,
        vtt_with("new tab").as_bytes(),
    ));

    assert_eq!(
        owner.attach_page_tracks(&new_view),
        PageTracksUpdate::Unchanged
    );
    assert_eq!(rows(&owner), ["English"]);

    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &new_view);
    assert_eq!(rows(&owner), ["Japanese"]);
    assert_eq!(displayed_texts_at(&owner, 2_000), ["new tab"]);
}

#[test]
fn without_media_the_shelf_changes_nothing() {
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    let mut shelf = TestShelf::new();
    let view = shelf.store(page_document(
        1,
        Some("en"),
        None,
        SHOWING,
        vtt_with("en").as_bytes(),
    ));
    assert_eq!(owner.attach_page_tracks(&view), PageTracksUpdate::Unchanged);
    assert_eq!(owner.menu_view().tracks.len(), 0);
}

#[test]
fn an_ad_on_screen_hides_the_layer_and_it_comes_back_after_the_ad() {
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            None,
            SHOWING,
            vtt_with("film").as_bytes(),
        )),
    );
    let placed = |owner: &mut SubtitleOwner| {
        owner
            .placed_overlay_for_frame(
                current(media, GENERATION),
                &window_frame(full_window_picture(), 2_000),
            )
            .is_some()
    };
    assert!(placed(&mut owner), "the cue is on the frame before the ad");
    let frames_before_ad = owner.shown_overlay_frames();

    owner.set_page_ad_presence(PageAdPresence::AdOnScreen);
    assert!(!placed(&mut owner), "no subtitles while the ad plate is up");
    assert_eq!(
        owner.shown_overlay_frames(),
        frames_before_ad,
        "a hidden frame is not counted"
    );
    // Выбор и дорожка не тронуты: реклама меняет только показ.
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));

    owner.set_page_ad_presence(PageAdPresence::NoAd);
    assert!(placed(&mut owner), "the cue is back after the ad");
    assert_eq!(owner.shown_overlay_frames(), frames_before_ad + 1);
}

#[test]
fn subtitles_the_viewer_turned_off_stay_off_in_the_next_generation_of_the_session() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    let view = shelf.store(page_document(
        1,
        Some("en"),
        None,
        SHOWING,
        vtt_with("film").as_bytes(),
    ));
    offer(&mut owner, &view);
    owner.set_visibility(SubtitleVisibility::Hidden);

    // Реклама посреди фильма, затем фильм — два новых медиа того же сеанса.
    for generation in [2, 3] {
        let media = instance(generation);
        owner.begin_media(media, SubtitleMediaSource::NotLocal);
        offer(&mut owner, &view);
        assert_eq!(rows(&owner), ["English"], "the page track is in the menu");
        assert_eq!(
            shown_row(&owner),
            None,
            "still off in generation {generation}"
        );
        let placed = owner
            .placed_overlay_for_frame(
                current(media, GENERATION),
                &window_frame(full_window_picture(), 2_000),
            )
            .is_some();
        assert!(!placed, "no subtitle layer in generation {generation}");
    }
}

#[test]
fn the_page_track_the_viewer_picked_stays_picked_in_the_next_generation_even_when_replaced() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    shelf.store(page_document(
        1,
        Some("en"),
        None,
        SHOWING,
        vtt_with("en").as_bytes(),
    ));
    let view = shelf.store(page_document(
        2,
        Some("ru"),
        None,
        NOT_SHOWING,
        vtt_with("ru").as_bytes(),
    ));
    offer(&mut owner, &view);
    click_menu_row(&mut owner, "Russian");
    settle(&mut owner);

    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &view);
    assert_eq!(shown_row(&owner).as_deref(), Some("Russian"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["ru"]);

    // Страница прислала новый документ той же русской дорожки — выбор указывает на него.
    let replaced = shelf.store(page_document(
        2,
        Some("ru"),
        None,
        NOT_SHOWING,
        vtt_with("ru again").as_bytes(),
    ));
    owner.begin_media(instance(3), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &replaced);
    assert_eq!(shown_row(&owner).as_deref(), Some("Russian"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["ru again"]);
}

#[test]
fn a_new_session_lets_the_page_showing_apply_again() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            None,
            SHOWING,
            vtt_with("old tab").as_bytes(),
        )),
    );
    owner.set_visibility(SubtitleVisibility::Hidden);

    // Другая вкладка в том же окне: новый склад, новое медиа.
    let mut next_session = shelf.of_next_session();
    let next_view = next_session.store(page_document(
        1,
        Some("en"),
        None,
        SHOWING,
        vtt_with("new tab").as_bytes(),
    ));
    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &next_view);
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["new tab"]);
}
