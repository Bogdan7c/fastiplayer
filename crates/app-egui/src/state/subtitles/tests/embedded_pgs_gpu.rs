//! PGS до пикселей: настоящая фикстура `pgs-basic.mkv` -> труба -> владелец -> поток-рисовальщик
//! -> настоящий GPU-проход -> readback. Тесты пропускаются (с сообщением), если на машине нет
//! GPU adapter-а, как остальные тесты цепочки субтитров.
//!
//! Система координат фикстуры 64x64; объекты (README фикстуры): 16x8 в (4, 40) красно-зелёный и
//! 12x6 в (44, 6) сине-белый (альфа половины 128). Квадратная система автора вписывается в
//! картинку видео `picture()` с сохранением пропорций (один масштаб + центрирование, как mpv),
//! поэтому ожидаемые пиксели считаются тем же вписыванием.

use render_core::PictureRect;

use super::embedded_pgs::*;
use super::embedded_support::*;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::new() {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the PGS subtitle test");
                return;
            }
        }
    };
}

/// Система координат PGS-фикстуры.
const REFERENCE_SIZE: f32 = 64.0;
/// Допуск по каналу: масштабирование и смешивание с альфой.
const CHANNEL_TOLERANCE: u8 = 8;

fn picture() -> PictureRect {
    letterboxed_rect(256, 144)
}

/// Пиксель окна по точке в системе автора: общий масштаб `min(w, h) / 64` и центрирование
/// вписанного квадрата внутри картинки видео.
fn window_pixel_of(reference_x: f32, reference_y: f32) -> (usize, usize) {
    let rect = picture();
    let scale = rect.width.min(rect.height) / REFERENCE_SIZE;
    let offset_x = (rect.width - REFERENCE_SIZE * scale) / 2.0;
    let offset_y = (rect.height - REFERENCE_SIZE * scale) / 2.0;
    let x = rect.x + offset_x + reference_x * scale;
    let y = rect.y + offset_y + reference_y * scale;
    (x as usize, y as usize)
}

fn pixel_at(frame: &[u8], point: (usize, usize)) -> [u8; 4] {
    let offset = (point.1 * 256 + point.0) * 4;
    [
        frame[offset],
        frame[offset + 1],
        frame[offset + 2],
        frame[offset + 3],
    ]
}

fn assert_close(actual: [u8; 4], expected: [u8; 4], what: &str) {
    for channel in 0..3 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= CHANNEL_TOLERANCE,
            "{what}: {actual:?} is not close to {expected:?}"
        );
    }
}

/// Кадр «видео» + субтитры в момент `pts_ms`.
fn render_at(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    pts_ms: u64,
) -> Vec<u8> {
    let rect = picture();
    let overlay = owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .cloned();
    harness.frame(rect, overlay.as_ref()).1
}

/// Центры половин объектов фикстуры в системе автора.
const RED_CENTER: (f32, f32) = (4.0 + 4.0, 40.0 + 4.0);
const GREEN_CENTER: (f32, f32) = (4.0 + 12.0, 40.0 + 4.0);
const BLUE_CENTER: (f32, f32) = (44.0 + 3.0, 6.0 + 3.0);
const WHITE_CENTER: (f32, f32) = (44.0 + 9.0, 6.0 + 3.0);

fn red_pixel(frame: &[u8]) -> [u8; 4] {
    pixel_at(frame, window_pixel_of(RED_CENTER.0, RED_CENTER.1))
}
fn green_pixel(frame: &[u8]) -> [u8; 4] {
    pixel_at(frame, window_pixel_of(GREEN_CENTER.0, GREEN_CENTER.1))
}
fn blue_pixel(frame: &[u8]) -> [u8; 4] {
    pixel_at(frame, window_pixel_of(BLUE_CENTER.0, BLUE_CENTER.1))
}
fn white_pixel(frame: &[u8]) -> [u8; 4] {
    pixel_at(frame, window_pixel_of(WHITE_CENTER.0, WHITE_CENTER.1))
}

#[test]
fn pgs_pictures_reach_the_gpu_frame_with_real_colours_only_while_shown() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    owner.deliver_feed_messages(fixture.feed_messages(media));

    let first = render_at(&mut owner, &mut harness, media, 1_000);
    let between = render_at(&mut owner, &mut harness, media, 1_700);
    let second = render_at(&mut owner, &mut harness, media, 2_300);
    let after_all = render_at(&mut owner, &mut harness, media, 2_800);
    let before_all = render_at(&mut owner, &mut harness, media, 200);

    assert_close(red_pixel(&first), [255, 0, 0, 255], "red half at 1.0 s");
    assert_close(green_pixel(&first), [0, 255, 0, 255], "green half at 1.0 s");
    assert_eq!(
        between, video_only,
        "the clear set at 1.5 s removes the picture"
    );
    assert_close(blue_pixel(&second), [0, 0, 255, 255], "blue half at 2.3 s");
    // Белая половина с альфой 128 (~50 %) поверх цвета «видео».
    assert_close(
        white_pixel(&second),
        [160, 192, 224, 255],
        "half-transparent white at 2.3 s",
    );
    assert_eq!(
        red_pixel(&second),
        pixel_at(&video_only, window_pixel_of(RED_CENTER.0, RED_CENTER.1)),
        "the first picture is gone while the second shows"
    );
    assert_eq!(
        after_all, video_only,
        "the clear set at 2.6 s removes the picture"
    );
    assert_eq!(before_all, video_only, "nothing before the first set");
}

#[test]
fn closing_a_picture_by_a_later_successor_keeps_it_on_screen_until_then_in_pixels() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let track = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_PICTURE]));
    let open_at_one = render_at(&mut owner, &mut harness, media, 1_000);
    let open_at_one_and_half_plus = render_at(&mut owner, &mut harness, media, 1_700);
    assert_close(red_pixel(&open_at_one), [255, 0, 0, 255], "open picture");
    assert_eq!(
        open_at_one, open_at_one_and_half_plus,
        "an open picture stays shown"
    );

    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_CLEAR]));
    let still_before_the_end = render_at(&mut owner, &mut harness, media, 1_000);
    let after_the_end = render_at(&mut owner, &mut harness, media, 1_700);

    assert_eq!(
        still_before_the_end, open_at_one,
        "shortening does not change what was visible before the new end"
    );
    assert_eq!(after_the_end, video_only);
}

#[test]
fn eviction_rebuild_keeps_the_current_picture_in_the_gpu_frame() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let track = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    observe_position(&mut owner, media, 2_300);
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE],
    ));
    let before = render_at(&mut owner, &mut harness, media, 2_300);
    assert_close(
        blue_pixel(&before),
        [0, 0, 255, 255],
        "blue before eviction",
    );
    let stored_bytes = owner
        .media
        .as_ref()
        .and_then(|media| media.embedded_pgs.track(owner_track_id(&owner, track)))
        .map(|state| state.estimated_bytes())
        .expect("pgs state");
    owner.settings.embedded_limits.pgs = owner
        .settings
        .embedded_limits
        .pgs
        .with_shown_budget_bytes(stored_bytes - 1);

    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[SECOND_CLEAR]));
    let after = render_at(&mut owner, &mut harness, media, 2_300);
    let evicted_first = render_at(&mut owner, &mut harness, media, 1_000);

    assert_eq!(
        after, before,
        "the picture active at the position survives the rebuild"
    );
    assert_eq!(
        evicted_first, video_only,
        "the farthest picture was evicted from the cache"
    );
}

#[test]
fn seeking_back_draws_the_cached_first_picture_without_re_reading() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let track = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE, SECOND_CLEAR],
    ));
    let at_second = render_at(&mut owner, &mut harness, media, 2_300);
    assert_close(blue_pixel(&at_second), [0, 0, 255, 255], "second picture");

    let packets_before = owner.embedded_feed_counters();
    let back_at_first = render_at(&mut owner, &mut harness, media, 1_000);

    assert_close(
        red_pixel(&back_at_first),
        [255, 0, 0, 255],
        "cached first picture",
    );
    assert_eq!(
        owner.embedded_feed_counters(),
        packets_before,
        "no packets were re-read"
    );
}

#[test]
fn hidden_subtitles_draw_only_the_forced_object_in_real_pixels() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    // Субтитры выключены, предпочитаемый язык совпал с языком дорожки (`eng`).
    let config = SubtitlesConfig {
        enabled: false,
        preferred_languages: "en".to_owned(),
        ..SubtitlesConfig::default()
    };
    let mut owner = embedded_owner(&config, media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    owner.deliver_feed_messages(fixture.feed_messages(media));

    let regular_moment = render_at(&mut owner, &mut harness, media, 1_000);
    let forced_moment = render_at(&mut owner, &mut harness, media, 2_300);

    assert_eq!(
        regular_moment, video_only,
        "the regular picture is not drawn while subtitles are off"
    );
    assert_close(
        blue_pixel(&forced_moment),
        [0, 0, 255, 255],
        "forced blue half",
    );
    assert_close(
        white_pixel(&forced_moment),
        [160, 192, 224, 255],
        "forced half-transparent white",
    );

    // Включили субтитры: обычная картинка теперь рисуется.
    owner.set_visibility(crate::state::subtitles::intents::SubtitleVisibility::Shown);
    let regular_when_shown = render_at(&mut owner, &mut harness, media, 1_000);
    assert_close(
        red_pixel(&regular_when_shown),
        [255, 0, 0, 255],
        "regular red half once subtitles are on",
    );
}
