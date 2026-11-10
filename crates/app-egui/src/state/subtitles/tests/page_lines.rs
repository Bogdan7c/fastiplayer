//! Строки меню субтитров страницы в владельце субтитров (сессия 15a2): меню CC показывает
//! строки страницы и не дублирует скачанное ею, а выбор строки превращает следующий
//! пришедший документ в выбор зрителя. Идёт настоящим путём: склад → `attach_page_tracks` →
//! фоновый разбор → реплики на кадре; действия зрителя — те же intent-методы, что у меню CC.

use std::sync::Arc;

use fastiplayer_config::SubtitlesConfig;

use super::page_support::*;
use super::track_selection::settle;
use super::web_support::{click_menu_row, menu_texts};
use super::*;
use crate::page_subtitles::{
    PageSubtitleDisplay, PageSubtitleDocument, PageSubtitleSource, PageSubtitleTrackNumber,
    PageSubtitleViewerChoice, PageTracksUpdate,
};
use crate::page_variants::{
    PageVariantAvailability, PageVariantIndex, PageVariantKind, PageVariantMenuRevision,
    PageVariantMenuView,
};
use crate::state::subtitles::menu_view::SubtitleMenuView;
use crate::state::subtitles::page_line_actions::expect_document_if_queued;
use crate::ui::player_controls::SubtitleControlAction;

const SHOWING: PageSubtitleDisplay = PageSubtitleDisplay::ShowingOnPage;
const NOT_SHOWING: PageSubtitleDisplay = PageSubtitleDisplay::NotShowingOnPage;

/// WebVTT с одной репликой от `start` до `end` секунд.
fn cue(start: u32, end: u32, text: &str) -> String {
    format!("WEBVTT\n\n00:00:{start:02}.000 --> 00:00:{end:02}.000\n{text}\n")
}

/// Документ, который скачала страница, с одной репликой с 1 по 4 с.
fn downloaded(
    track: u32,
    language: &str,
    display: PageSubtitleDisplay,
    text: &str,
) -> PageSubtitleDocument {
    page_document(
        track,
        Some(language),
        None,
        display,
        cue(1, 4, text).as_bytes(),
    )
}

/// Тот же документ, но взятый из элемента `<track>`, а не скачанный страницей.
fn from_track_element(
    track: u32,
    language: &str,
    display: PageSubtitleDisplay,
    text: &str,
) -> PageSubtitleDocument {
    PageSubtitleDocument {
        source: PageSubtitleSource::PageElement,
        ..downloaded(track, language, display, text)
    }
}

/// Ревизия меню в этих тестах одна: сверка ревизий проверяется в окне и в сеансе.
const MENU_REVISION: PageVariantMenuRevision = PageVariantMenuRevision::new(1);

/// Меню субтитров страницы с отмеченной строкой `selected`.
fn page_menu(labels: &[&str], selected: Option<usize>) -> PageVariantMenuView {
    PageVariantMenuView::new(
        PageVariantKind::Subtitles,
        MENU_REVISION,
        labels.iter().map(|label| Arc::from(*label)).collect(),
        selected.map(PageVariantIndex::new),
    )
}

fn menu_with(owner: &SubtitleOwner, menu: &PageVariantMenuView) -> SubtitleMenuView {
    owner.menu_view_with_page_menu(Some(menu), PageVariantAvailability::Selectable)
}

/// Подписи строк страницы и их отметки.
fn page_rows(view: &SubtitleMenuView) -> Vec<(String, bool)> {
    view.page_lines
        .iter()
        .map(|line| (line.label.to_string(), line.is_selected))
        .collect()
}

fn page_track_report(number: u32) -> Option<PageSubtitleViewerChoice> {
    Some(PageSubtitleViewerChoice::PageTrack(
        PageSubtitleTrackNumber::new(number),
    ))
}

/// Владелец, где зритель сам выбрал английскую дорожку страницы (номер 1), и склад с ней.
fn viewer_picked_english() -> (SubtitleOwner, TestShelf) {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(downloaded(1, "en", SHOWING, "HELLO")),
    );
    click_menu_row(&mut owner, "English");
    settle(&mut owner);
    // Отчёт первого выбора зритель уже отдал странице.
    let _ = owner.take_viewer_page_choice_report();
    (owner, shelf)
}

// ---- меню CC ------------------------------------------------------------------------------

#[test]
fn page_lines_come_first_and_downloads_of_the_page_are_not_listed_twice() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(downloaded(1, "en", SHOWING, "HELLO")),
    );
    offer(
        &mut owner,
        &shelf.store(from_track_element(2, "fr", NOT_SHOWING, "SALUT")),
    );
    let menu = page_menu(&["English", "German (auto-translated)"], Some(0));

    let view = menu_with(&owner, &menu);

    assert_eq!(
        page_rows(&view),
        [
            ("English".to_owned(), true),
            ("German (auto-translated)".to_owned(), false)
        ]
    );
    assert_eq!(
        view.page_lines
            .iter()
            .map(|line| line.is_auto_translation)
            .collect::<Vec<_>>(),
        [false, true]
    );
    // Скачанный страницей английский представляет строка; дорожка из `<track>` остаётся.
    let listed: Vec<String> = view
        .tracks
        .iter()
        .map(|track| {
            crate::subtitle_message::track_text(
                &track.label,
                crate::subtitle_message::TrackNumber::from_menu_index(0),
            )
        })
        .collect();
    assert_eq!(listed, ["French"]);
    assert!(view.has_tracks && view.has_menu_entries());

    // Страница меню не прислала: обе дорожки на месте, строк нет.
    let without_menu = owner.menu_view_with_page_menu(None, PageVariantAvailability::Selectable);
    assert!(without_menu.page_lines.is_empty());
    assert_eq!(without_menu.tracks.len(), 2);
}

#[test]
fn lines_alone_make_the_menu_available_without_a_single_track() {
    let owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let menu = page_menu(&["English", "German"], None);

    let view = menu_with(&owner, &menu);

    assert!(!view.has_tracks, "no document has been downloaded yet");
    assert!(
        view.has_menu_entries(),
        "the CC button is shown for the lines"
    );
    assert_eq!(view.page_lines.len(), 2);
    assert!(view.page_lines.iter().all(|line| !line.is_selected));
    // Без меню страницы и без дорожек кнопки CC нет, как раньше.
    assert!(
        !owner
            .menu_view_with_page_menu(None, PageVariantAvailability::Selectable)
            .has_menu_entries()
    );
}

#[test]
fn the_page_mark_follows_visibility_and_yields_to_a_listed_track_the_viewer_picked() {
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(downloaded(1, "en", SHOWING, "HELLO")),
    );
    offer(
        &mut owner,
        &shelf.store(from_track_element(2, "fr", NOT_SHOWING, "SALUT")),
    );
    let menu = page_menu(&["English", "German"], Some(0));
    assert_eq!(
        page_rows(&menu_with(&owner, &menu))[0],
        ("English".to_owned(), true)
    );

    // Зритель выбрал дорожку из списка: две радио-отметки в одном меню были бы враньём.
    click_menu_row(&mut owner, "French");
    settle(&mut owner);
    let view = menu_with(&owner, &menu);
    assert!(view.page_lines.iter().all(|line| !line.is_selected));
    assert!(view.tracks.iter().any(|track| track.is_selected));

    // «Выкл»: ни одна строка не отмечена, пока наши субтитры не показываются.
    owner.apply_control_action(SubtitleControlAction::Disable);
    let hidden = menu_with(&owner, &menu);
    assert!(hidden.page_lines.iter().all(|line| !line.is_selected));
    assert!(hidden.tracks.iter().all(|track| !track.is_selected));
}

#[test]
fn the_ad_availability_is_carried_to_the_lines() {
    let owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    let menu = page_menu(&["English"], None);
    let view =
        owner.menu_view_with_page_menu(Some(&menu), PageVariantAvailability::BlockedDuringAd);
    assert_eq!(
        view.page_lines_availability,
        PageVariantAvailability::BlockedDuringAd
    );
}

// ---- «жду документ этой строки» -------------------------------------------------------------

#[test]
fn the_document_that_follows_a_line_choice_becomes_the_viewers_choice_and_is_reported() {
    let (mut owner, mut shelf) = viewer_picked_english();
    owner.expect_page_document();
    // Пока документа нет, прежний показ не трогается.
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["HELLO"]);
    assert!(owner.is_waiting_for_page_document());

    let update = offer(
        &mut owner,
        &shelf.store(downloaded(2, "de", SHOWING, "HALLO")),
    );

    assert_eq!(update, PageTracksUpdate::Changed);
    assert_eq!(shown_row(&owner).as_deref(), Some("German"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["HALLO"]);
    assert_eq!(owner.take_viewer_page_choice_report(), page_track_report(2));
    assert!(!owner.is_waiting_for_page_document(), "the wait is over");
}

#[test]
fn without_a_line_choice_a_showing_document_does_not_beat_the_viewers_own_pick() {
    let (mut owner, mut shelf) = viewer_picked_english();

    offer(
        &mut owner,
        &shelf.store(downloaded(2, "de", SHOWING, "HALLO")),
    );

    assert_eq!(
        shown_row(&owner).as_deref(),
        Some("English"),
        "the old order of strength: the viewer beats the page's showing"
    );
    assert_eq!(owner.take_viewer_page_choice_report(), None);
}

#[test]
fn a_replacement_of_a_track_the_viewer_left_is_shown_when_the_line_is_chosen_again() {
    let (mut owner, mut shelf) = viewer_picked_english();
    // en -> de: документ немецкого пришёл, зритель на нём.
    owner.expect_page_document();
    offer(
        &mut owner,
        &shelf.store(downloaded(2, "de", SHOWING, "HALLO")),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("German"));
    let _ = owner.take_viewer_page_choice_report();

    // Страница сама перекачала английский, зритель строку не выбирал: показ не меняется.
    offer(
        &mut owner,
        &shelf.store(downloaded(1, "en", SHOWING, "HELLO TWICE")),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("German"));

    // de -> en: зритель выбрал строку английского, YouTube скачал документ заново, и он
    // заменил прежнюю дорожку на месте (тот же номер).
    owner.expect_page_document();
    offer(
        &mut owner,
        &shelf.store(downloaded(1, "en", SHOWING, "HELLO AGAIN")),
    );

    assert_eq!(shown_row(&owner).as_deref(), Some("English"));
    assert_eq!(displayed_texts_at(&owner, 2_000), ["HELLO AGAIN"]);
    assert_eq!(owner.take_viewer_page_choice_report(), page_track_report(1));
}

#[test]
fn documents_seen_before_the_choice_never_answer_it_even_for_a_new_generation() {
    let (mut owner, mut shelf) = viewer_picked_english();
    // Немецкий пришёл раньше выбора и страница его показывала: зритель выбрал английский.
    offer(
        &mut owner,
        &shelf.store(downloaded(2, "de", SHOWING, "HALLO")),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));

    owner.expect_page_document();
    // Тот же склад ещё раз и новое поколение медиа (после рекламы): всё это уже видели.
    assert_eq!(
        owner.attach_page_tracks(&shelf.view()),
        PageTracksUpdate::Unchanged
    );
    owner.begin_media(instance(2), SubtitleMediaSource::NotLocal);
    offer(&mut owner, &shelf.view());

    assert_eq!(shown_row(&owner).as_deref(), Some("English"));
    assert!(
        owner.is_waiting_for_page_document(),
        "the answer has not come yet"
    );
}

#[test]
fn only_shown_downloads_of_the_page_answer_the_choice() {
    let (mut owner, mut shelf) = viewer_picked_english();
    owner.expect_page_document();

    offer(
        &mut owner,
        &shelf.store(from_track_element(2, "de", SHOWING, "FROM ELEMENT")),
    );
    offer(
        &mut owner,
        &shelf.store(downloaded(3, "fr", NOT_SHOWING, "NOT SHOWN")),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("English"));
    assert!(owner.is_waiting_for_page_document());

    offer(
        &mut owner,
        &shelf.store(downloaded(4, "es", SHOWING, "HOLA")),
    );
    assert_eq!(shown_row(&owner).as_deref(), Some("Spanish"));
    assert_eq!(owner.take_viewer_page_choice_report(), page_track_report(4));
}

#[test]
fn the_wait_ends_when_the_viewer_picks_another_track_or_turns_subtitles_off() {
    for ending in ["another track", "off"] {
        let (mut owner, mut shelf) = viewer_picked_english();
        offer(
            &mut owner,
            &shelf.store(from_track_element(2, "fr", NOT_SHOWING, "SALUT")),
        );
        owner.expect_page_document();
        match ending {
            "another track" => click_menu_row(&mut owner, "French"),
            _ => {
                let _ = owner.apply_control_action(SubtitleControlAction::Disable);
            }
        }
        settle(&mut owner);
        assert!(!owner.is_waiting_for_page_document(), "{ending}");
        let chosen = shown_row(&owner);

        offer(
            &mut owner,
            &shelf.store(downloaded(3, "de", SHOWING, "HALLO")),
        );

        assert_eq!(
            shown_row(&owner),
            chosen,
            "{ending}: a late document changes nothing"
        );
    }
}

#[test]
fn a_new_session_forgets_the_wait() {
    let (mut owner, shelf) = viewer_picked_english();
    owner.expect_page_document();
    let mut next_session = shelf.of_next_session();

    offer(
        &mut owner,
        &next_session.store(downloaded(1, "de", SHOWING, "HALLO")),
    );

    assert!(!owner.is_waiting_for_page_document());
    assert_eq!(
        owner.take_viewer_page_choice_report(),
        None,
        "a document of a new page is not the answer to the old page's line"
    );
}

#[test]
fn waiting_does_not_change_the_menu_text_of_the_tracks() {
    // Страховка «ожидание не владеет дорожками»: список дорожек до и после выбора строки один.
    let (mut owner, _shelf) = viewer_picked_english();
    let before = menu_texts(&owner);
    owner.expect_page_document();
    assert_eq!(menu_texts(&owner), before);
}

#[test]
fn only_a_queued_request_makes_the_owner_wait_for_the_document() {
    use crate::state::PageVariantSelectOutcome;

    let refusals = [
        PageVariantSelectOutcome::RefusedDuringAd,
        PageVariantSelectOutcome::NoMenu,
        PageVariantSelectOutcome::StaleMenu,
        PageVariantSelectOutcome::NoSuchOption,
        PageVariantSelectOutcome::NotPageLed,
    ];
    for outcome in refusals {
        let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
        expect_document_if_queued(outcome, &mut owner);
        assert!(!owner.is_waiting_for_page_document(), "{outcome:?}");
    }
    let mut owner = browser_owner(&SubtitlesConfig::default(), instance(1));
    expect_document_if_queued(PageVariantSelectOutcome::Queued, &mut owner);
    assert!(owner.is_waiting_for_page_document());
}
