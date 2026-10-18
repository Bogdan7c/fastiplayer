//! Размещение PGS до пикселей (решение владельца, сессия 11): нижняя картинка (основная группа)
//! поднимается над панелью и следует вертикальному сдвигу пользователя, но не выходит за
//! картинку видео; верхняя (фиксированная группа) стоит на авторском месте; масштаб и
//! горизонтальный сдвиг не применяются. Настоящая фикстура `pgs-basic.mkv`: нижний объект
//! 16x8 в (4, 40) на 0.5–1.5 с, верхний 12x6 в (44, 6) на 2.0–2.6 с; система автора 64x64
//! растянута на картинку видео. Тесты пропускаются без GPU.

use super::ass_gpu_support::*;
use super::embedded_support::*;
use super::gpu_chain::Harness;
use super::*;
use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the PGS placement test");
                return;
            }
        }
    };
}

/// Нижняя картинка на экране.
const BOTTOM_PICTURE_MS: u64 = 1_000;
/// Верхняя картинка на экране.
const TOP_PICTURE_MS: u64 = 2_300;
/// Сдвиг пользователя `position_vertical_percent` = 6 даёт нулевой сдвиг (число пресета).
const NEUTRAL_VERTICAL_PERCENT: i16 = 6;

fn is_blue(pixel: [u8; 4]) -> bool {
    pixel[0] <= 40 && pixel[1] <= 40 && pixel[2] >= 215
}

/// Нижний объект фикстуры: красная и зелёная половины.
fn is_bottom_object(pixel: [u8; 4]) -> bool {
    is_red(pixel) || is_green(pixel)
}

/// Владелец PGS-фикстуры с субтитрами «включены»; пакеты доставлены.
fn pgs_owner(config: &SubtitlesConfig) -> SubtitleOwner {
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let mut owner = embedded_owner(config, media, &fixture);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    owner
}

fn config_with_vertical(percent: i16) -> SubtitlesConfig {
    SubtitlesConfig {
        position_vertical_percent: percent,
        ..enabled_config()
    }
}

fn render(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    rect: render_core::PictureRect,
    obstruction: BottomControlsObstruction,
    pts_ms: u64,
) -> Vec<u8> {
    owner.observe_bottom_controls_obstruction(obstruction);
    render_placed(owner, harness, instance(2), rect, pts_ms)
}

fn bottom_object_bounds(pixels: &[u8]) -> PixelBounds {
    bounds_of(pixels, WINDOW_WIDTH, is_bottom_object).expect("the bottom picture is drawn")
}

#[test]
fn panel_lifts_the_bottom_picture_above_it_and_leaves_the_top_picture_pixel_identical() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = pgs_owner(&config_with_vertical(NEUTRAL_VERTICAL_PERCENT));
    let author = render(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        BOTTOM_PICTURE_MS,
    );
    let author_bounds = bottom_object_bounds(&author);
    let panel_top = author_bounds.max_y as i32 - 10;

    let with_panel = render(
        &mut owner,
        &mut harness,
        rect,
        controls_from(panel_top),
        BOTTOM_PICTURE_MS,
    );

    let lifted = bottom_object_bounds(&with_panel);
    assert!(
        (lifted.max_y as i32) < panel_top,
        "bottom picture is above the panel: {lifted:?}, panel at {panel_top}"
    );
    assert_eq!(lifted.height(), author_bounds.height(), "size is untouched");
    assert_eq!(lifted.min_x, author_bounds.min_x, "no horizontal move");

    // Верхняя картинка (фиксированная группа) не двигается даже огромной панелью.
    let top_author = render(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        TOP_PICTURE_MS,
    );
    let top_under_panel = render(
        &mut owner,
        &mut harness,
        rect,
        controls_from(0),
        TOP_PICTURE_MS,
    );
    assert!(count_of(&top_author, is_blue) > 100, "top picture is drawn");
    assert_eq!(
        top_under_panel, top_author,
        "top picture stays where authored"
    );
}

#[test]
fn hidden_panel_returns_the_bottom_picture_to_the_author_place() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = pgs_owner(&config_with_vertical(NEUTRAL_VERTICAL_PERCENT));
    let author = render(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        BOTTOM_PICTURE_MS,
    );
    let shown = render(
        &mut owner,
        &mut harness,
        rect,
        controls_from(bottom_object_bounds(&author).max_y as i32 - 10),
        BOTTOM_PICTURE_MS,
    );
    assert_ne!(shown, author, "visible panel moves the picture");

    let hidden_again = render(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        BOTTOM_PICTURE_MS,
    );

    assert_eq!(hidden_again, author);
}

#[test]
fn user_vertical_offset_moves_only_the_bottom_picture() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut neutral = pgs_owner(&config_with_vertical(NEUTRAL_VERTICAL_PERCENT));
    // +20 п.п. = 0.2 высоты картинки (360) = 72 px вверх.
    let mut raised = pgs_owner(&config_with_vertical(NEUTRAL_VERTICAL_PERCENT + 20));
    let author = render(
        &mut neutral,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        BOTTOM_PICTURE_MS,
    );
    let moved = render(
        &mut raised,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        BOTTOM_PICTURE_MS,
    );

    let (before, after) = (bottom_object_bounds(&author), bottom_object_bounds(&moved));
    assert_eq!(after.min_y + 72, before.min_y, "moved 72 px up");
    assert_eq!(after.max_y + 72, before.max_y);
    assert_eq!((after.min_x, after.max_x), (before.min_x, before.max_x));

    let top_author = render(
        &mut neutral,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        TOP_PICTURE_MS,
    );
    let top_with_offset = render(
        &mut raised,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        TOP_PICTURE_MS,
    );
    assert_eq!(
        top_with_offset, top_author,
        "top picture ignores the offset"
    );
}

#[test]
fn extreme_user_offsets_keep_the_picture_inside_the_video_never_on_the_bars() {
    let mut harness = gpu_or_skip!();
    let rect = letterboxed_window_picture();
    let (picture_top, picture_bottom) =
        (rect.y.round() as u32, (rect.y + rect.height).round() as u32);
    // Самый большой сдвиг вверх (95 %) и вниз (-40 %).
    for percent in [95, -40] {
        let mut owner = pgs_owner(&config_with_vertical(percent));
        let pixels = render(
            &mut owner,
            &mut harness,
            rect,
            BottomControlsObstruction::Absent,
            BOTTOM_PICTURE_MS,
        );
        let bounds = bottom_object_bounds(&pixels);
        assert!(
            bounds.min_y >= picture_top && bounds.max_y < picture_bottom,
            "percent {percent}: {bounds:?} must be inside {picture_top}..{picture_bottom}"
        );
        for y in (0..picture_top).chain(picture_bottom..WINDOW_HEIGHT) {
            assert!(
                !row_has_non_black(&pixels, y),
                "row {y} is a bar and must stay black (percent {percent})"
            );
        }
    }
    // Предел вверх достигнут: верх группы стоит на верхнем краю картинки.
    let mut owner = pgs_owner(&config_with_vertical(95));
    let pixels = render(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
        BOTTOM_PICTURE_MS,
    );
    assert_eq!(bottom_object_bounds(&pixels).min_y, picture_top);
}

#[test]
fn text_scale_and_horizontal_offset_do_not_affect_pictures() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut plain = pgs_owner(&config_with_vertical(NEUTRAL_VERTICAL_PERCENT));
    let mut styled = pgs_owner(&SubtitlesConfig {
        text_size_percent: 200,
        position_horizontal_percent: 100,
        ..config_with_vertical(NEUTRAL_VERTICAL_PERCENT)
    });
    for pts_ms in [BOTTOM_PICTURE_MS, TOP_PICTURE_MS] {
        let expected = render(
            &mut plain,
            &mut harness,
            rect,
            BottomControlsObstruction::Absent,
            pts_ms,
        );
        let actual = render(
            &mut styled,
            &mut harness,
            rect,
            BottomControlsObstruction::Absent,
            pts_ms,
        );
        assert!(
            count_of(&expected, |p| is_bottom_object(p) || is_blue(p)) > 100,
            "picture is drawn at {pts_ms} ms"
        );
        assert_eq!(
            actual, expected,
            "scale and horizontal offset ignored at {pts_ms} ms"
        );
    }
}
