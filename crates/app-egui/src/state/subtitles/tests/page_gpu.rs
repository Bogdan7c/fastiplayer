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
