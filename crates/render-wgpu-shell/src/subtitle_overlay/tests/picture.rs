//! Растровые реплики (PGS) на GPU: настоящая реплика -> растеризатор -> RGBA-слой -> пиксели.
//!
//! Проверяем итог на экране (readback), а не вызовы: точные цвета, смешивание полупрозрачной
//! картинки, порядок относительно текста, сдвиги групп, учёт загрузок и поведение на лимите
//! текстуры GPU.

use std::num::NonZeroU32;
use std::sync::Arc;

use subtitle_core::PictureRect as VisibleRect;
use subtitle_core::{
    IndexedPicture, PaletteColor, PictureEncoding, PictureForcedFlag, PicturePalette,
    PicturePosition, PictureReferenceSize, SubtitleEventId, SubtitlePictureCue,
};
use subtitle_pgs::stream_builder::encode_rle;

use super::group_offsets::full_picture;
use super::*;

/// Опорный размер холста PGS-реплик: равен размеру цели, поэтому масштаб 1:1.
const REFERENCE: (u32, u32) = (TARGET_WIDTH, TARGET_HEIGHT);
/// Допуск на округление unorm8 при смешивании на GPU.
const PICTURE_TOLERANCE: i32 = 2;

fn non_zero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

/// Однотонная картинка `size` цвета `fill` с левым верхним углом `origin` (в опорных пикселях).
pub(crate) fn solid_cue(
    origin: (u32, u32),
    size: (u16, u16),
    fill: PaletteColor,
) -> SubtitlePictureCue {
    let indices = vec![1_u8; usize::from(size.0) * usize::from(size.1)];
    let encoded = encode_rle(&indices, size.0, size.1);
    let picture = IndexedPicture::new(
        non_zero(u32::from(size.0)),
        non_zero(u32::from(size.1)),
        PictureEncoding::HdmvRle(Arc::from(encoded)),
    );
    let mut entries = [PaletteColor::TRANSPARENT; PicturePalette::LEN];
    entries[1] = fill;
    SubtitlePictureCue::new(
        PictureReferenceSize::new(non_zero(REFERENCE.0), non_zero(REFERENCE.1)),
        PicturePosition {
            x: origin.0,
            y: origin.1,
        },
        VisibleRect::new(
            0,
            0,
            non_zero(u32::from(size.0)),
            non_zero(u32::from(size.1)),
        ),
        Arc::new(picture),
        Arc::new(PicturePalette::new(entries)),
        PictureForcedFlag::Regular,
    )
    .expect("visible region is the whole picture")
}

pub(crate) fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> PaletteColor {
    PaletteColor {
        red,
        green,
        blue,
        alpha,
    }
}

/// Идентификатор события, полученный так же, как у настоящей дорожки (вставкой в таймлайн).
fn event_id(content: SubtitleContent) -> SubtitleEventId {
    let mut timeline = SubtitleTimeline::new();
    let event = SubtitleEvent::new(
        SubtitleTime::from_millis(0),
        SubtitleTime::from_millis(5),
        SubtitleLayer::DEFAULT,
        content,
    )
    .expect("interval");
    let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
        panic!("event must be inserted");
    };
    id
}

/// Сцена кадра: необязательный текст (источник 1) и картинки (источники 100, 101, ...).
/// Одинаковые источники в одном растеризаторе дают одни и те же буферы из кэша.
pub(crate) fn render_scene(
    rasterizer: &mut SubtitleRasterizer,
    canvas: CanvasSize,
    text: Option<&str>,
    pictures: &[(u64, &SubtitlePictureCue)],
) -> SubtitleOverlay {
    let text_cue = SubtitleTextCue {
        text: StyledText::new(vec![TextSpan::text(
            text.unwrap_or(""),
            SpanStyle::default(),
        )]),
        placement: CuePlacement::default(),
    };
    // Источник зависит от длины текста: разные тексты не должны делить кэш растеризатора
    // (ключ кэша - источник и событие, а не сам текст).
    let text_source = 1 + text.map_or(0, str::len) as u64;
    let text_key = CueKey::new(
        CueSourceId::new(text_source),
        event_id(SubtitleContent::Text(text_cue.clone())),
    );
    let mut cues: Vec<RenderableCue<'_>> = Vec::new();
    if text.is_some() {
        cues.push(RenderableCue::text(text_key, &text_cue));
    }
    for &(source, picture) in pictures {
        let key = CueKey::new(
            CueSourceId::new(source),
            event_id(SubtitleContent::Picture(picture.clone())),
        );
        cues.push(RenderableCue::picture(key, picture));
    }
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::ZERO,
            cues: &cues,
            canvas,
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

pub(crate) fn target_canvas() -> CanvasSize {
    CanvasSize::new(TARGET_WIDTH, TARGET_HEIGHT).expect("canvas")
}

fn frame_of(overlay: &SubtitleOverlay) -> SubtitleOverlayFrame<'_> {
    frame_with_offsets(overlay, SubtitleLayerOffsets::NONE)
}

fn frame_with_offsets(
    overlay: &SubtitleOverlay,
    layer_offsets: SubtitleLayerOffsets,
) -> SubtitleOverlayFrame<'_> {
    SubtitleOverlayFrame {
        overlay,
        picture_rect: full_picture(),
        layer_offsets,
    }
}

fn assert_close(actual: [u8; 4], expected: [u8; 4], tolerance: i32, place: &str) {
    for channel in 0..4 {
        let difference = (i32::from(actual[channel]) - i32::from(expected[channel])).abs();
        assert!(
            difference <= tolerance,
            "{place}: {actual:?} differs from {expected:?} in channel {channel}"
        );
    }
}

/// Все пиксели прямоугольника `x, y, w, h` равны `expected` с допуском.
fn assert_rect_color(pixels: &[u8], area: (u32, u32, u32, u32), expected: [u8; 4], tolerance: i32) {
    let (x0, y0, width, height) = area;
    for y in y0..y0 + height {
        for x in x0..x0 + width {
            assert_close(
                pixel(pixels, x, y),
                expected,
                tolerance,
                &format!("({x},{y})"),
            );
        }
    }
}

/// Сколько пикселей прямоугольника отличается от того же места в `reference`.
fn differing_pixels(pixels: &[u8], reference: &[u8], area: (u32, u32, u32, u32)) -> usize {
    let (x0, y0, width, height) = area;
    let mut count = 0;
    for y in y0..y0 + height {
        for x in x0..x0 + width {
            if pixel(pixels, x, y) != pixel(reference, x, y) {
                count += 1;
            }
        }
    }
    count
}

/// Строка текста внизу кадра занимает эту область (с запасом).
const TEXT_AREA: (u32, u32, u32, u32) = (0, 90, TARGET_WIDTH, TARGET_HEIGHT - 90);

#[test]
fn opaque_picture_lands_on_its_exact_screen_pixels() {
    let mut harness = gpu_or_skip!();
    let cue = solid_cue((20, 30), (10, 6), rgba(200, 100, 50, 255));
    let overlay = render_scene(&mut rasterizer(), target_canvas(), None, &[(100, &cue)]);
    let (_, video_only) = harness.frame(None, full_view());

    let (outcome, pixels) = harness.frame(Some(&frame_of(&overlay)), full_view());

    assert_eq!(outcome, SubtitleOverlayOutcome::Drawn { bitmaps: 1 });
    assert_rect_color(&pixels, (20, 30, 10, 6), [200, 100, 50, 255], 1);
    // Сразу за краями картинки остаётся нетронутое видео.
    for (x, y) in [(19, 30), (30, 30), (20, 29), (20, 36), (30, 35), (0, 0)] {
        assert_eq!(pixel(&pixels, x, y), pixel(&video_only, x, y), "({x},{y})");
    }
}

#[test]
fn semi_transparent_picture_blends_premultiplied_over_the_video() {
    let mut harness = gpu_or_skip!();
    let cue = solid_cue((40, 40), (8, 8), rgba(255, 0, 0, 128));
    let overlay = render_scene(&mut rasterizer(), target_canvas(), None, &[(100, &cue)]);
    let bitmap = &overlay.bitmaps()[0];
    let texel = bitmap.rgba_row(0).expect("first row");
    // Контракт растеризатора: цвет уже умножен на альфу.
    assert_eq!(&texel[..4], &[128, 0, 0, 128]);
    let (_, video_only) = harness.frame(None, full_view());
    let video = pixel(&video_only, 42, 42);

    let (_, pixels) = harness.frame(Some(&frame_of(&overlay)), full_view());

    // out = source + video * (1 - source.a), source = texel.
    let inverse = 255 - i32::from(texel[3]);
    let expected = [0, 1, 2, 3].map(|channel| {
        let blended = i32::from(texel[channel]) + i32::from(video[channel]) * inverse / 255;
        u8::try_from(blended.min(255)).expect("clamped to u8")
    });
    assert_rect_color(&pixels, (40, 40, 8, 8), expected, PICTURE_TOLERANCE);
    assert_ne!(pixel(&pixels, 42, 42), video, "blend must change the video");
    assert_eq!(pixel(&pixels, 42, 42)[3], 255, "framebuffer stays opaque");
}

#[test]
fn picture_and_text_are_both_visible_and_do_not_disturb_each_other() {
    let mut harness = gpu_or_skip!();
    let cue = solid_cue((10, 10), (20, 10), rgba(0, 255, 0, 255));
    let mut rasterizer = rasterizer();
    let text_only = render_scene(&mut rasterizer, target_canvas(), Some("Hello"), &[]);
    let mixed = render_scene(
        &mut rasterizer,
        target_canvas(),
        Some("Hello"),
        &[(100, &cue)],
    );
    assert_eq!(mixed.bitmaps().len(), text_only.bitmaps().len() + 1);
    let (_, video_only) = harness.frame(None, full_view());
    let (_, text_pixels) = harness.frame(Some(&frame_of(&text_only)), full_view());
    assert!(differing_pixels(&text_pixels, &video_only, TEXT_AREA) > 0);

    let (outcome, mixed_pixels) = harness.frame(Some(&frame_of(&mixed)), full_view());

    assert_eq!(
        outcome,
        SubtitleOverlayOutcome::Drawn {
            bitmaps: mixed.bitmaps().len()
        }
    );
    assert_rect_color(&mixed_pixels, (10, 10, 20, 10), [0, 255, 0, 255], 1);
    assert_eq!(
        differing_pixels(&mixed_pixels, &text_pixels, TEXT_AREA),
        0,
        "text pixels are identical with and without the picture"
    );
}

#[test]
fn picture_is_drawn_over_overlapping_text_following_list_order() {
    let mut harness = gpu_or_skip!();
    // Плашка закрывает всю нижнюю полосу, где стоит текст; растеризатор ставит картинки
    // в списке после текста, значит плашка лежит сверху.
    let cover = solid_cue((0, 90), (TARGET_WIDTH as u16, 54), rgba(255, 0, 255, 255));
    let mut rasterizer = rasterizer();
    let text_only = render_scene(&mut rasterizer, target_canvas(), Some("Hello"), &[]);
    let covered = render_scene(
        &mut rasterizer,
        target_canvas(),
        Some("Hello"),
        &[(100, &cover)],
    );
    let (_, video_only) = harness.frame(None, full_view());
    let (_, text_pixels) = harness.frame(Some(&frame_of(&text_only)), full_view());
    assert!(
        differing_pixels(&text_pixels, &video_only, TEXT_AREA) > 0,
        "text really is in the area"
    );

    let (_, pixels) = harness.frame(Some(&frame_of(&covered)), full_view());

    assert_rect_color(&pixels, TEXT_AREA, [255, 0, 255, 255], 1);
}

#[test]
fn layer_offsets_move_a_fixed_group_picture_and_ignore_the_main_offset() {
    let mut harness = gpu_or_skip!();
    let cue = solid_cue((20, 60), (10, 6), rgba(10, 200, 30, 255));
    let overlay = render_scene(&mut rasterizer(), target_canvas(), None, &[(100, &cue)]);
    assert_eq!(
        overlay.bitmaps()[0].placement_group(),
        subtitle_raster::OverlayPlacementGroup::Fixed,
        "растровые реплики растеризатор относит к фиксированной группе"
    );
    let (_, video_only) = harness.frame(None, full_view());
    let offsets = |main_y: f32, fixed_y: f32| SubtitleLayerOffsets {
        main: SubtitleLayerOffset { x: 0.0, y: main_y },
        fixed: SubtitleLayerOffset { x: 0.0, y: fixed_y },
    };

    // Сдвиг основной группы картинку фиксированной группы не трогает.
    let (_, main_only) = harness.frame(
        Some(&frame_with_offsets(&overlay, offsets(40.0, 0.0))),
        full_view(),
    );
    assert_rect_color(&main_only, (20, 60, 10, 6), [10, 200, 30, 255], 1);

    // Сдвиг фиксированной группы поднимает картинку на 20 пикселей.
    let (_, fixed_moved) = harness.frame(
        Some(&frame_with_offsets(&overlay, offsets(0.0, -20.0))),
        full_view(),
    );
    assert_rect_color(&fixed_moved, (20, 40, 10, 6), [10, 200, 30, 255], 1);
    assert_eq!(pixel(&fixed_moved, 25, 62), pixel(&video_only, 25, 62));

    let stats = harness.renderer.upload_stats();
    assert_eq!(
        stats.picture_atlas_uploads, 1,
        "offsets change only the uniform"
    );
    assert_eq!(stats.picture_bitmap_uploads, 1);
}

#[test]
fn unchanged_overlay_uploads_nothing_and_each_layer_reuploads_only_its_own_change() {
    let mut harness = gpu_or_skip!();
    let green = solid_cue((10, 10), (20, 10), rgba(0, 255, 0, 255));
    let blue = solid_cue((10, 10), (20, 10), rgba(0, 0, 255, 255));
    let mut rasterizer = rasterizer();
    let first = render_scene(
        &mut rasterizer,
        target_canvas(),
        Some("Hello"),
        &[(100, &green)],
    );

    let (_, first_pixels) = harness.frame(Some(&frame_of(&first)), full_view());
    let after_first = harness.renderer.upload_stats();
    assert_eq!(after_first.picture_atlas_uploads, 1);
    assert_eq!(after_first.picture_bitmap_uploads, 1);
    assert_eq!(after_first.picture_atlas_recreations, 1);
    assert_eq!(after_first.atlas_uploads, 1);

    let (_, second_pixels) = harness.frame(Some(&frame_of(&first)), full_view());
    assert_eq!(harness.renderer.upload_stats(), after_first, "zero uploads");
    assert_eq!(first_pixels, second_pixels);

    // Меняется только картинка: атлас текста не пишется заново.
    let picture_changed = render_scene(
        &mut rasterizer,
        target_canvas(),
        Some("Hello"),
        &[(101, &blue)],
    );
    let (_, blue_pixels) = harness.frame(Some(&frame_of(&picture_changed)), full_view());
    let after_picture_change = harness.renderer.upload_stats();
    assert_eq!(after_picture_change.picture_atlas_uploads, 2);
    assert_eq!(after_picture_change.atlas_uploads, 1);
    assert_rect_color(&blue_pixels, (10, 10, 20, 10), [0, 0, 255, 255], 1);

    // Меняется только текст: атлас картинок не пишется заново.
    let text_changed = render_scene(
        &mut rasterizer,
        target_canvas(),
        Some("Different words"),
        &[(101, &blue)],
    );
    let (_, text_changed_pixels) = harness.frame(Some(&frame_of(&text_changed)), full_view());
    let after_text_change = harness.renderer.upload_stats();
    assert_eq!(after_text_change.atlas_uploads, 2);
    assert_eq!(after_text_change.picture_atlas_uploads, 2);
    assert_ne!(blue_pixels, text_changed_pixels);
    assert_rect_color(&text_changed_pixels, (10, 10, 20, 10), [0, 0, 255, 255], 1);
}

#[test]
fn text_only_overlays_never_create_the_rgba_atlas() {
    let mut harness = gpu_or_skip!();
    let overlay = render_scene(&mut rasterizer(), target_canvas(), Some("Hello"), &[]);

    let (outcome, _) = harness.frame(Some(&frame_of(&overlay)), full_view());

    assert!(matches!(outcome, SubtitleOverlayOutcome::Drawn { .. }));
    let stats = harness.renderer.upload_stats();
    assert_eq!(stats.atlas_uploads, 1, "text was uploaded");
    assert_eq!(stats.picture_atlas_recreations, 0);
    assert_eq!(stats.picture_atlas_uploads, 0);
    assert_eq!(stats.picture_bitmap_uploads, 0);
    assert!(harness.renderer.picture_layer.is_none());
}

#[test]
fn pictures_that_do_not_fit_the_atlas_are_skipped_while_text_still_draws() {
    // Лимит стороны 256: атлас 256x256 = 65536 пикселей, а две картинки 256x144 = 73728.
    let Some(mut harness) = Harness::with_texture_limit(Some(256)) else {
        eprintln!("skip: no GPU adapter for subtitle overlay test");
        return;
    };
    let wide_a = solid_cue((0, 0), (TARGET_WIDTH as u16, 144), rgba(255, 0, 0, 255));
    let wide_b = solid_cue((0, 0), (TARGET_WIDTH as u16, 144), rgba(0, 0, 255, 255));
    let mut rasterizer = rasterizer();
    let text_only = render_scene(&mut rasterizer, target_canvas(), Some("Hello"), &[]);
    let scene = render_scene(
        &mut rasterizer,
        target_canvas(),
        Some("Hello"),
        &[(100, &wide_a), (101, &wide_b)],
    );
    let (_, text_pixels) = harness.frame(Some(&frame_of(&text_only)), full_view());

    let (outcome, pixels) = harness.frame(Some(&frame_of(&scene)), full_view());

    assert_eq!(
        outcome,
        SubtitleOverlayOutcome::Drawn {
            bitmaps: text_only.bitmaps().len()
        },
        "only the text bitmaps are drawn"
    );
    assert_eq!(pixels, text_pixels, "the skipped pictures leave no trace");
    let stats = harness.renderer.upload_stats();
    assert_eq!(stats.skipped_picture_bitmaps_too_large, 2);
    assert_eq!(stats.picture_bitmap_uploads, 0);
}

#[test]
fn a_single_picture_larger_than_the_texture_limit_is_skipped_but_neighbours_draw() {
    let Some(mut harness) = Harness::with_texture_limit(Some(256)) else {
        eprintln!("skip: no GPU adapter for subtitle overlay test");
        return;
    };
    // Холст шире экрана и лимита: картинка 300 пикселей в ширину не влезет ни в один атлас.
    let canvas = CanvasSize::new(512, 144).expect("canvas");
    let wide = solid_cue_in((0, 0), (300, 10), rgba(255, 0, 0, 255), (512, 144));
    let small = solid_cue_in((20, 60), (10, 6), rgba(0, 255, 0, 255), (512, 144));
    let overlay = render_scene(
        &mut rasterizer(),
        canvas,
        None,
        &[(100, &wide), (101, &small)],
    );
    assert_eq!(overlay.bitmaps().len(), 2);
    let (_, video_only) = harness.frame(None, full_view());

    let (outcome, pixels) = harness.frame(Some(&frame_of(&overlay)), full_view());

    assert_eq!(outcome, SubtitleOverlayOutcome::Drawn { bitmaps: 1 });
    assert_rect_color(&pixels, (20, 60, 10, 6), [0, 255, 0, 255], 1);
    // Широкая полоса сверху не нарисована.
    assert_eq!(pixel(&pixels, 5, 5), pixel(&video_only, 5, 5));
    assert_eq!(
        harness
            .renderer
            .upload_stats()
            .skipped_picture_bitmaps_too_large,
        1
    );
}

/// Как [`solid_cue`], но с произвольным опорным размером кадра.
fn solid_cue_in(
    origin: (u32, u32),
    size: (u16, u16),
    fill: PaletteColor,
    reference: (u32, u32),
) -> SubtitlePictureCue {
    let base = solid_cue(origin, size, fill);
    SubtitlePictureCue::new(
        PictureReferenceSize::new(non_zero(reference.0), non_zero(reference.1)),
        base.origin(),
        base.visible_region(),
        Arc::clone(base.picture()),
        Arc::clone(base.palette()),
        base.forced(),
    )
    .expect("visible region is the whole picture")
}
