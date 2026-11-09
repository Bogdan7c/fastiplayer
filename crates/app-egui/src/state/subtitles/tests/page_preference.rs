//! Подсказка страницы о субтитрах и отчёт выбора зрителя (протокол 1.7, 14a) в владельце
//! субтитров. Идёт настоящим путём: снимок склада с подсказкой → `attach_page_tracks` →
//! фоновый разбор → реплики на кадре; действия зрителя — те же intent-методы, что у меню CC и
//! клавиш C/V.

use fastiplayer_config::SubtitlesConfig;

use super::page_support::*;
use super::track_selection::settle;
use super::web_support::{click_menu_row, menu_texts};
use super::*;
use crate::page_subtitles::{
    PageSubtitleDisplay, PageSubtitlePreference, PageSubtitleTrackNumber, PageSubtitleViewerChoice,
    PageTracksUpdate,
};
use crate::state::subtitles::intents::AttachOutcome;
use crate::ui::player_controls::SubtitleControlAction;

const SHOWING: PageSubtitleDisplay = PageSubtitleDisplay::ShowingOnPage;
const NOT_SHOWING: PageSubtitleDisplay = PageSubtitleDisplay::NotShowingOnPage;

fn page_track(number: u32) -> PageSubtitlePreference {
    PageSubtitlePreference::PageTrack(PageSubtitleTrackNumber::new(number))
}

fn rows(owner: &SubtitleOwner) -> Vec<String> {
    menu_texts(owner)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// Склад с английской дорожкой, которую страница показывает, и русской, которую нет.
fn english_showing_and_russian(shelf: &mut TestShelf) {
    shelf.store(page_document(
        1,
        Some("en"),
        None,
        SHOWING,
        vtt_with("en").as_bytes(),
    ));
    shelf.store(page_document(
        2,
        Some("ru"),
        None,
        NOT_SHOWING,
        vtt_with("ru").as_bytes(),
    ));
}

#[test]
fn the_page_preference_beats_the_track_the_page_is_showing() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    english_showing_and_russian(&mut shelf);

    offer(&mut owner, &shelf.prefer(page_track(2)));

    assert_eq!(rows(&owner), ["English", "Russian"]);
    assert_eq!(shown_row(&owner).as_deref(), Some("Russian"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["ru"]);
    // Подсказка — не действие зрителя: ни конфигу, ни странице сообщать нечего.
    assert_eq!(owner.take_enabled_preference_change(), None);
    assert_eq!(owner.take_viewer_page_choice_report(), None);
}

#[test]
fn a_preference_that_arrives_after_the_tracks_still_switches_to_its_track() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    english_showing_and_russian(&mut shelf);
    offer(&mut owner, &shelf.view());
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));

    // Документов не прибавилось — изменилась только подсказка.
    assert_eq!(
        offer(&mut owner, &shelf.prefer(page_track(2))),
        PageTracksUpdate::Changed
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("Russian"));
    assert_eq!(
        offer(&mut owner, &shelf.view()),
        PageTracksUpdate::Unchanged,
        "the same word again changes nothing"
    );
}

#[test]
fn the_preference_waits_for_its_track_and_takes_it_when_it_arrives() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    shelf.prefer(page_track(3));
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
    // Дорожки 3 ещё нет: пока действует `showing`.
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));

    offer(
        &mut owner,
        &shelf.store(page_document(
            3,
            Some("de"),
            None,
            NOT_SHOWING,
            vtt_with("de").as_bytes(),
        )),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("German"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["de"]);
}

#[test]
fn off_keeps_subtitles_off_although_the_page_is_showing_a_track() {
    let config = SubtitlesConfig {
        enabled: true,
        ..SubtitlesConfig::default()
    };
    let mut owner = browser_owner(&config, instance(1));
    let mut shelf = TestShelf::new();
    english_showing_and_russian(&mut shelf);

    offer(&mut owner, &shelf.prefer(PageSubtitlePreference::Off));

    assert_eq!(rows(&owner), ["English", "Russian"]);
    assert_eq!(shown_row(&owner), None);
    assert!(displayed_texts_at(&owner, 2_000).is_empty());
    // «Выключено» — только для медиа сеанса: конфиг («включено») подсказка не трогает.
    assert_eq!(owner.default_visibility, SubtitleVisibility::Shown);
}

#[test]
fn the_viewer_beats_the_preference_and_it_applies_to_the_next_generation_until_then() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    english_showing_and_russian(&mut shelf);
    let view = shelf.prefer(page_track(2));
    offer(&mut owner, &view);

    // Новое поколение того же сеанса: подсказка действует снова.
    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &view);
    assert_eq!(shown_row(&owner).as_deref(), Some("Russian"));

    // Зритель выбрал английскую: подсказка (и новая подсказка) его не перебивает.
    click_menu_row(&mut owner, "English");
    settle(&mut owner);
    offer(&mut owner, &shelf.prefer(PageSubtitlePreference::Off));
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));
    owner.begin_media(instance(3), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &shelf.view());
    assert_eq!(
        shown_row(&owner).as_deref(),
        Some("English"),
        "the viewer's pick survives the generation, the page's word does not beat it"
    );
}

#[test]
fn viewer_actions_report_page_tracks_and_off_but_automatic_choices_do_not() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    english_showing_and_russian(&mut shelf);
    offer(&mut owner, &shelf.prefer(page_track(1)));
    // Автовыбор, `showing` и подсказка отчёта не дают.
    assert_eq!(owner.take_viewer_page_choice_report(), None);

    click_menu_row(&mut owner, "Russian");
    assert_eq!(
        owner.take_viewer_page_choice_report(),
        Some(PageSubtitleViewerChoice::PageTrack(
            PageSubtitleTrackNumber::new(2)
        ))
    );
    assert_eq!(owner.take_viewer_page_choice_report(), None, "taken once");

    // Клавиша C: выключить — «off», включить снова — показанная дорожка страницы.
    owner.apply_control_action(SubtitleControlAction::ToggleVisibility);
    assert_eq!(
        owner.take_viewer_page_choice_report(),
        Some(PageSubtitleViewerChoice::Off)
    );
    owner.apply_control_action(SubtitleControlAction::ToggleVisibility);
    assert_eq!(
        owner.take_viewer_page_choice_report(),
        Some(PageSubtitleViewerChoice::PageTrack(
            PageSubtitleTrackNumber::new(2)
        ))
    );
}

#[test]
fn the_viewers_own_file_reports_nothing() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    english_showing_and_russian(&mut shelf);
    offer(&mut owner, &shelf.view());
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("Own.srt");
    std::fs::write(&path, "1\n00:00:01,000 --> 00:00:04,000\nown\n").expect("write own file");

    let outcome = owner.attach_file(&path);
    settle(&mut owner);

    assert!(matches!(outcome, AttachOutcome::Attached(_)), "{outcome:?}");
    assert_eq!(displayed_texts_at(&owner, 2_000), ["own"]);
    assert_eq!(
        owner.take_viewer_page_choice_report(),
        None,
        "the extension cannot remember our own file"
    );
}
