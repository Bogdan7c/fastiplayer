//! Субтитры страницы до пикселей (мост, 13b1): документ склада → дорожка, которую показывала
//! страница → фоновый разбор → настоящий рисовальщик → настоящий GPU-проход → readback.
//! Во время рекламы страницы пикселей субтитров нет. Тесты пропускаются (с сообщением), если
//! на машине нет GPU adapter-а.

use fastiplayer_config::SubtitlesConfig;

use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::page_support::*;
use super::*;
use crate::page_subtitles::PageSubtitleDisplay;
use crate::state::subtitles::owner_page::PageAdPresence;
use crate::ui::player_controls::SubtitleControlAction;

/// Светлый пиксель текста: красный канал «видео» (64) и затемнённой плашки ниже порога.
fn is_light(pixel: [u8; 4]) -> bool {
    pixel[0] > 150
}

fn frame_at(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    pts_millis: u64,
) -> Vec<u8> {
    render_placed(owner, harness, media, full_window_picture(), pts_millis)
}

#[test]
fn a_document_the_page_was_showing_is_drawn_during_its_cue_and_not_after() {
    let Some(mut harness) = Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) else {
        eprintln!("skip: no GPU adapter for the page subtitle test");
        return;
    };
    let media = instance(1);
    // Субтитры в настройках выключены: включает их «скачала — значит включено» страницы.
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let before_page = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(
        bounds_of(&before_page, WINDOW_WIDTH, is_light).is_none(),
        "no ink before the page sends anything"
    );

    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            Some("English"),
            PageSubtitleDisplay::ShowingOnPage,
            vtt_with("HELLO FROM THE PAGE").as_bytes(),
        )),
    );

    let during_cue = frame_at(&mut owner, &mut harness, media, 2_000);
    let after_cue = frame_at(&mut owner, &mut harness, media, 5_000);
    assert!(
        bounds_of(&during_cue, WINDOW_WIDTH, is_light).is_some(),
        "the page cue is drawn"
    );
    assert!(
        bounds_of(&after_cue, WINDOW_WIDTH, is_light).is_none(),
        "and gone after its end"
    );
}

#[test]
fn no_subtitle_pixels_while_an_ad_plate_is_on_screen() {
    let Some(mut harness) = Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) else {
        eprintln!("skip: no GPU adapter for the page subtitle test");
        return;
    };
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            None,
            PageSubtitleDisplay::ShowingOnPage,
            vtt_with("FILM DIALOGUE").as_bytes(),
        )),
    );
    let before_ad = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(bounds_of(&before_ad, WINDOW_WIDTH, is_light).is_some());

    owner.set_page_ad_presence(PageAdPresence::AdOnScreen);
    let during_ad = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(
        bounds_of(&during_ad, WINDOW_WIDTH, is_light).is_none(),
        "the ad hides the subtitles"
    );

    owner.set_page_ad_presence(PageAdPresence::NoAd);
    let after_ad = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(
        bounds_of(&after_ad, WINDOW_WIDTH, is_light).is_some(),
        "the subtitles are back after the ad"
    );
}

#[test]
fn after_a_line_choice_the_document_that_arrives_is_drawn_and_without_it_the_old_pick_stays() {
    let Some(mut harness) = Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) else {
        eprintln!("skip: no GPU adapter for the page subtitle test");
        return;
    };
    let media = instance(1);
    let mut owner = browser_owner(&SubtitlesConfig::default(), media);
    let mut shelf = TestShelf::new();
    // Английский (реплика 1–4 с): зритель выбрал его сам, и страница его показывает.
    offer(
        &mut owner,
        &shelf.store(page_document(
            1,
            Some("en"),
            Some("English"),
            PageSubtitleDisplay::ShowingOnPage,
            vtt_with("HELLO").as_bytes(),
        )),
    );
    owner.apply_control_action(SubtitleControlAction::SelectTrack(
        owner.menu_view().tracks[0].id,
    ));
    // Немецкий отличим по времени: его реплика на 6–8 с.
    let german_cue = "WEBVTT\n\n00:00:06.000 --> 00:00:08.000\nHALLO\n";
    let german = |track| {
        page_document(
            track,
            Some("de"),
            Some("German"),
            PageSubtitleDisplay::ShowingOnPage,
            german_cue.as_bytes(),
        )
    };

    // Документ, которого зритель не просил, прежний выбор не перебивает: на 7 с чернил нет.
    offer(&mut owner, &shelf.store(german(2)));
    let unrequested = frame_at(&mut owner, &mut harness, media, 7_000);
    assert!(
        bounds_of(&unrequested, WINDOW_WIDTH, is_light).is_none(),
        "the viewer's own pick still wins over an unrequested document"
    );

    // Зритель выбрал строку страницы; документ второй раз (замена) показывается и рисуется.
    owner.expect_page_document();
    let before_answer = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(
        bounds_of(&before_answer, WINDOW_WIDTH, is_light).is_some(),
        "the previous subtitles stay on screen while the page prepares the document"
    );
    offer(&mut owner, &shelf.store(german(2)));
    let answered = frame_at(&mut owner, &mut harness, media, 7_000);
    let old_cue_gone = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(
        bounds_of(&answered, WINDOW_WIDTH, is_light).is_some(),
        "the document that answered the line is drawn"
    );
    assert!(
        bounds_of(&old_cue_gone, WINDOW_WIDTH, is_light).is_none(),
        "and the old track is no longer shown"
    );
}
