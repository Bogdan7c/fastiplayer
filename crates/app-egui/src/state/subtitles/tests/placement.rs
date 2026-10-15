//! Размещение слоя субтитров до пикселей: владелец -> сдвиг -> настоящий GPU-проход -> readback.
//!
//! Проверяется то, что видит зритель: по умолчанию субтитры на прежних строках; панель
//! управления поднимает их только пока она мешает; позиция может увести их на чёрную полосу;
//! горизонталь не выпускает слой за картинку; при смене размера окна позиция сохраняется
//! пропорционально. Тесты пропускаются (с сообщением) без GPU adapter-а.

use fastiplayer_config::SubtitlesConfig;
use render_core::PictureLayout;
use render_core::PictureSize;

use super::gpu_chain::Harness;
use super::*;
use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;

const GENERATION: u64 = 5;

macro_rules! gpu_or_skip {
    ($width:expr, $height:expr) => {
        match Harness::with_size($width, $height) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the subtitle placement test");
                return;
            }
        }
    };
}

/// Границы пикселей, изменённых субтитрами: `(min_x, min_y, max_x, max_y)` включительно.
fn changed_bounds(
    with_subtitles: &[u8],
    video_only: &[u8],
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            if with_subtitles[offset..offset + 4] != video_only[offset..offset + 4] {
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((min_x, min_y, max_x, max_y)) => {
                        (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
                    }
                });
            }
        }
    }
    bounds
}

/// Кадр на всю область окна: картинка `rect`, область видео - всё окно.
fn window_frame(rect: PictureRect, width: u32, height: u32) -> SubmittedFrameForSubtitles {
    SubmittedFrameForSubtitles::for_test_with_viewport(
        Duration::from_millis(2_000),
        GENERATION,
        rect,
        RenderViewport::full_surface(width, height),
    )
}

/// Владелец с загруженной дорожкой `TWO_CUE_SRT` и позицией из конфига.
fn owner_with_position(
    media: MediaInstanceId,
    horizontal_percent: u8,
    vertical_percent: i16,
) -> (SubtitleOwner, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let mut owner = owner_with_config(&SubtitlesConfig {
        enabled: true,
        position_horizontal_percent: horizontal_percent,
        position_vertical_percent: vertical_percent,
        ..SubtitlesConfig::default()
    });
    begin_and_wait(&mut owner, media, &video);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    (owner, directory)
}

/// Кадр окна: размещение владельцем -> GPU -> пиксели.
fn render_placed(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    rect: PictureRect,
) -> Vec<u8> {
    let frame = window_frame(rect, harness.width, harness.height);
    let placed = owner
        .placed_overlay_for_frame(current(media, GENERATION), &frame)
        .expect("overlay inside the cue");
    let (overlay, offset) = (placed.overlay.clone(), placed.layer_offset);
    harness.frame_with_offset(rect, Some(&overlay), offset).1
}

#[test]
fn default_style_keeps_rows_and_controls_lift_the_subtitle_only_while_they_obstruct() {
    let mut harness = gpu_or_skip!(256, 144);
    let media = instance(1);
    let (mut owner, _directory) = owner_with_position(media, 50, 6);
    let rect = letterboxed_rect(256, 144);
    let (_, video_only) = harness.frame(rect, None);

    // Эталон прежнего поведения: тот же overlay без какого-либо сдвига.
    let plain = owner
        .overlay_for_frame(current(media, GENERATION), &window_frame(rect, 256, 144))
        .expect("overlay")
        .clone();
    let (_, unshifted) = harness.frame(rect, Some(&plain));
    let original = changed_bounds(&unshifted, &video_only, 256, 144).expect("subtitle pixels");

    // Стиль по умолчанию, панели нет: пиксели побитно те же.
    let default_pixels = render_placed(&mut owner, &mut harness, media, rect);
    assert_eq!(
        default_pixels, unshifted,
        "default style must not move anything"
    );
    assert_eq!(
        harness.renderer.upload_stats().atlas_uploads,
        1,
        "same overlay: no second upload"
    );

    // Панель сверху ниже субтитра: ничего не двигается (не мешает).
    owner.observe_bottom_controls_obstruction(BottomControlsObstruction::TopEdge {
        top_px: 144,
        clearance_px: 8,
    });
    assert_eq!(
        render_placed(&mut owner, &mut harness, media, rect),
        unshifted
    );

    // Панель закрывает субтитр: низ слоя ровно на зазоре над панелью.
    let panel_top = (original.3 - 4) as i32;
    owner.observe_bottom_controls_obstruction(BottomControlsObstruction::TopEdge {
        top_px: panel_top,
        clearance_px: 8,
    });
    let lifted = render_placed(&mut owner, &mut harness, media, rect);
    let lifted_bounds = changed_bounds(&lifted, &video_only, 256, 144).expect("lifted pixels");
    assert!(
        (lifted_bounds.3 as i32) < panel_top - 8,
        "subtitle ink ends above the panel clearance: {lifted_bounds:?}, panel {panel_top}"
    );
    let layer = owner.last_placed_layer_rect().expect("placed layer");
    assert_eq!(layer.y + layer.height as i32, panel_top - 8);

    // Панель уехала: субтитр вернулся на исходные строки.
    owner.observe_bottom_controls_obstruction(BottomControlsObstruction::Absent);
    assert_eq!(
        render_placed(&mut owner, &mut harness, media, rect),
        unshifted
    );

    // Ни одна из перестановок не перерастеризовала и не перезагрузила маски.
    assert_eq!(owner.presentation_stats().rasterizer_calls, 1);
    assert_eq!(harness.renderer.upload_stats().atlas_uploads, 1);
}

#[test]
fn negative_vertical_position_puts_the_subtitle_on_the_bar_below_the_picture() {
    let mut harness = gpu_or_skip!(256, 144);
    let media = instance(1);
    let (mut owner, _directory) = owner_with_position(media, 50, -20);
    let rect = letterboxed_rect(256, 144);
    let picture_bottom = (rect.y + rect.height).round() as u32;
    let (_, video_only) = harness.frame(rect, None);

    let pixels = render_placed(&mut owner, &mut harness, media, rect);
    let (_, min_y, _, max_y) = changed_bounds(&pixels, &video_only, 256, 144).expect("pixels");
    assert!(
        min_y >= picture_bottom - 4,
        "subtitle starts at the picture bottom (row {min_y}, picture bottom {picture_bottom})"
    );
    assert!(
        max_y >= picture_bottom,
        "subtitle pixels are on the bar (row {max_y})"
    );
    assert!(max_y < 144, "and stay inside the window");
    let layer = owner.last_placed_layer_rect().expect("layer");
    assert!(layer.y + layer.height as i32 <= 144);
}

#[test]
fn rightmost_position_stops_at_the_picture_edge_not_at_the_window_edge() {
    let (width, height) = (256, 144);
    let mut harness = gpu_or_skip!(width, height);
    let media = instance(1);
    let (mut owner, _directory) = owner_with_position(media, 100, 6);
    // Картинка 4:3 в окне 16:9: колонки по бокам.
    let rect = PictureLayout::fit(
        PictureSize::new(4, 3),
        RenderViewport::full_surface(width, height),
    )
    .expect("pillarbox")
    .rect();
    let picture_right = (rect.x + rect.width).round() as u32;
    assert!(picture_right < width, "fixture has a side bar");
    let (_, video_only) = harness.frame(rect, None);

    let pixels = render_placed(&mut owner, &mut harness, media, rect);
    let (_, _, max_x, _) = changed_bounds(&pixels, &video_only, width, height).expect("pixels");
    assert!(
        max_x < picture_right,
        "ink stays inside the picture (col {max_x}, picture right {picture_right})"
    );
    let layer = owner.last_placed_layer_rect().expect("layer");
    assert_eq!(layer.x + layer.width as i32, picture_right as i32);
}

#[test]
fn window_resize_keeps_the_position_proportional() {
    let media = instance(1);
    let measure = |width: u32, height: u32| -> Option<(f32, f32)> {
        let mut harness = Harness::with_size(width, height)?;
        let (mut owner, _directory) = owner_with_position(media, 25, 40);
        let rect = letterboxed_rect(width, height);
        let (_, video_only) = harness.frame(rect, None);
        let pixels = render_placed(&mut owner, &mut harness, media, rect);
        let (min_x, _, max_x, max_y) =
            changed_bounds(&pixels, &video_only, width, height).expect("pixels");
        // Центр слоя по горизонтали и низ по вертикали - в долях картинки.
        let centre_x = ((min_x + max_x) as f32 / 2.0 - rect.x) / rect.width;
        let bottom_y = (max_y as f32 - rect.y) / rect.height;
        Some((centre_x, bottom_y))
    };
    let (Some(small), Some(large)) = (measure(256, 144), measure(512, 288)) else {
        eprintln!("skip: no GPU adapter for the subtitle placement test");
        return;
    };
    assert!(
        (small.0 - large.0).abs() < 0.03 && (small.1 - large.1).abs() < 0.03,
        "relative position drifted: small {small:?}, large {large:?}"
    );
    // Позиция действительно пользовательская: центр левее середины, низ выше пресета.
    assert!(
        small.0 < 0.4,
        "centre is left of the picture middle: {small:?}"
    );
    assert!(
        small.1 < 0.7,
        "bottom is well above the preset spot: {small:?}"
    );
}

#[test]
fn no_overlay_means_no_remembered_layer_rect() {
    let media = instance(1);
    let (mut owner, _directory) = owner_with_position(media, 50, 6);
    let rect = letterboxed_rect(256, 144);
    let inside_cue = window_frame(rect, 256, 144);
    assert!(
        owner
            .placed_overlay_for_frame(current(media, GENERATION), &inside_cue)
            .is_some()
    );
    assert!(owner.last_placed_layer_rect().is_some());

    // Между репликами слоя нет, и рамке правки нечего показывать.
    let between_cues = SubmittedFrameForSubtitles::for_test_with_viewport(
        Duration::from_millis(4_000),
        GENERATION,
        rect,
        RenderViewport::full_surface(256, 144),
    );
    assert!(
        owner
            .placed_overlay_for_frame(current(media, GENERATION), &between_cues)
            .is_none()
    );
    assert_eq!(owner.last_placed_layer_rect(), None);

    // Другое media: тоже ничего.
    assert!(
        owner
            .placed_overlay_for_frame(current(instance(2), GENERATION), &inside_cue)
            .is_none()
    );
}
