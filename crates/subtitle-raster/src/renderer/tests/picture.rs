//! Растровые реплики (PGS) через публичный растеризатор: реплика на входе - пиксели на выходе.

use std::num::NonZeroU32;
use std::sync::Arc;

use subtitle_core::{
    IndexedPicture, PaletteColor, PictureEncoding, PictureForcedFlag, PicturePalette,
    PicturePosition, PictureRect, PictureReferenceSize, SubtitlePictureCue,
};
use subtitle_pgs::stream_builder::encode_rle;

use super::*;
use crate::overlay::{BitmapPixelFormat, OverlayChange, OverlayPlacementGroup, PixelPoint};

/// Индексная картинка теста: `indices` по строкам, `palette` - пары (индекс, цвет).
struct PictureSpec<'a> {
    reference: (u32, u32),
    origin: (u32, u32),
    size: (u16, u16),
    indices: &'a [u8],
    palette: &'a [(u8, PaletteColor)],
    forced: PictureForcedFlag,
}

fn non_zero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

fn color(red: u8, green: u8, blue: u8, alpha: u8) -> PaletteColor {
    PaletteColor {
        red,
        green,
        blue,
        alpha,
    }
}

/// Строит растровую реплику; видимая область - вся картинка.
fn picture_cue(spec: &PictureSpec<'_>) -> SubtitlePictureCue {
    let (width, height) = spec.size;
    let encoded = encode_rle(spec.indices, width, height);
    let picture = IndexedPicture::new(
        non_zero(u32::from(width)),
        non_zero(u32::from(height)),
        PictureEncoding::HdmvRle(Arc::from(encoded)),
    );
    let mut entries = [PaletteColor::TRANSPARENT; PicturePalette::LEN];
    for &(index, entry) in spec.palette {
        entries[usize::from(index)] = entry;
    }
    SubtitlePictureCue::new(
        PictureReferenceSize::new(non_zero(spec.reference.0), non_zero(spec.reference.1)),
        PicturePosition {
            x: spec.origin.0,
            y: spec.origin.1,
        },
        PictureRect::new(
            0,
            0,
            non_zero(u32::from(width)),
            non_zero(u32::from(height)),
        ),
        Arc::new(picture),
        Arc::new(PicturePalette::new(entries)),
        spec.forced,
    )
    .expect("visible region is the whole picture")
}

/// Однотонная картинка `width x height` цвета `fill` на позиции `origin` кадра 1920x1080.
fn solid(origin: (u32, u32), size: (u16, u16), fill: PaletteColor) -> SubtitlePictureCue {
    let indices = vec![1_u8; usize::from(size.0) * usize::from(size.1)];
    picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin,
        size,
        indices: &indices,
        palette: &[(1, fill)],
        forced: PictureForcedFlag::Regular,
    })
}

fn canvas_of(width: u32, height: u32) -> CanvasSize {
    CanvasSize::new(width, height).expect("test canvas")
}

/// Рисует запрос из готовых реплик на холсте `canvas`.
fn render_cues(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[RenderableCue<'_>],
    canvas: CanvasSize,
) -> SubtitleOverlay {
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::ZERO,
            cues,
            canvas,
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: crate::ass::AssRenderOptions::default(),
        })
        .clone()
}

/// Единственная картинка-Picture оверлея.
fn only_picture(overlay: &SubtitleOverlay) -> &SubtitleBitmap {
    let pictures = of_kind(overlay, SubtitleBitmapKind::Picture);
    assert_eq!(pictures.len(), 1, "expected exactly one picture bitmap");
    pictures[0]
}

/// Пиксель `[r, g, b, a]` картинки в её собственных координатах.
fn texel(bitmap: &SubtitleBitmap, x: u32, y: u32) -> [u8; 4] {
    let row = bitmap.rgba_row(y).expect("row inside picture");
    let start = x as usize * 4;
    [row[start], row[start + 1], row[start + 2], row[start + 3]]
}

fn keys(count: usize) -> Vec<TestCue> {
    let texts: Vec<&str> = vec!["text"; count];
    cues(7, &texts)
}

#[test]
fn picture_at_identity_scale_lands_at_its_position_with_premultiplied_pixels() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    // Три пикселя в ряд: непрозрачный красный, полупрозрачный, прозрачный.
    let cue = picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin: (100, 900),
        size: (3, 1),
        indices: &[1, 2, 3],
        palette: &[(1, color(255, 0, 0, 255)), (2, color(200, 100, 50, 128))],
        forced: PictureForcedFlag::Regular,
    });
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(1920, 1080),
    );

    let picture = only_picture(&overlay);
    assert_eq!(picture.pixel_format(), BitmapPixelFormat::PremultipliedRgba);
    assert_eq!(picture.origin(), PixelPoint { x: 100, y: 900 });
    assert_eq!((picture.width(), picture.height()), (3, 1));
    assert_eq!(texel(picture, 0, 0), [255, 0, 0, 255]);
    // round(200*128/255)=100, round(100*128/255)=50, round(50*128/255)=25.
    assert_eq!(texel(picture, 1, 0), [100, 50, 25, 128]);
    assert_eq!(texel(picture, 2, 0), [0, 0, 0, 0]);
    // У RGBA-картинки нет маски: старый доступ не отдаёт цветные байты как покрытие.
    assert!(picture.coverage().is_empty());
    assert_eq!(picture.cue_key(), Some(key));
    assert_eq!(
        picture.color(),
        subtitle_core::SubtitleColor::opaque(255, 255, 255)
    );
}

#[test]
fn downscaling_to_half_halves_position_and_size_and_averages_colours() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    // 2x2: красный, синий / синий, красный -> один пиксель, среднее четырёх.
    let cue = picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin: (100, 900),
        size: (2, 2),
        indices: &[1, 2, 2, 1],
        palette: &[(1, color(255, 0, 0, 255)), (2, color(0, 0, 255, 255))],
        forced: PictureForcedFlag::Regular,
    });
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(960, 540),
    );

    let picture = only_picture(&overlay);
    assert_eq!(picture.origin(), PixelPoint { x: 50, y: 450 });
    assert_eq!((picture.width(), picture.height()), (1, 1));
    // (255 + 255 + 0 + 0 + 2) / 4 = 128 по красному и синему, альфа 255.
    assert_eq!(texel(picture, 0, 0), [128, 0, 128, 255]);
}

#[test]
fn downscaled_big_object_keeps_a_half_size_and_edge_colours() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    // 8x4: левая половина красная, правая синяя; уменьшение вдвое даёт 4x2 с теми же половинами.
    let indices: Vec<u8> = (0..4).flat_map(|_| [1, 1, 1, 1, 2, 2, 2, 2]).collect();
    let cue = picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin: (200, 800),
        size: (8, 4),
        indices: &indices,
        palette: &[(1, color(255, 0, 0, 255)), (2, color(0, 0, 255, 255))],
        forced: PictureForcedFlag::Regular,
    });
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(960, 540),
    );
    let picture = only_picture(&overlay);
    assert_eq!(picture.origin(), PixelPoint { x: 100, y: 400 });
    assert_eq!((picture.width(), picture.height()), (4, 2));
    assert_eq!(texel(picture, 1, 0), [255, 0, 0, 255]);
    assert_eq!(texel(picture, 2, 1), [0, 0, 255, 255]);
}

#[test]
fn upscaling_doubles_position_and_size_and_replicates_pixels() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    let cue = picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin: (100, 900),
        size: (2, 1),
        indices: &[1, 2],
        palette: &[(1, color(255, 0, 0, 255)), (2, color(0, 255, 0, 255))],
        forced: PictureForcedFlag::Regular,
    });
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(3840, 2160),
    );
    let picture = only_picture(&overlay);
    assert_eq!(picture.origin(), PixelPoint { x: 200, y: 1800 });
    assert_eq!((picture.width(), picture.height()), (4, 2));
    for y in 0..2 {
        assert_eq!(texel(picture, 0, y), [255, 0, 0, 255]);
        assert_eq!(texel(picture, 1, y), [255, 0, 0, 255]);
        assert_eq!(texel(picture, 2, y), [0, 255, 0, 255]);
        assert_eq!(texel(picture, 3, y), [0, 255, 0, 255]);
    }
}

#[test]
fn picture_sticking_out_of_the_canvas_is_clipped_keeping_the_visible_pixels() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    // 4x4 у правого нижнего угла: видна верхняя левая 2x2; левая колонка - красная, правая синяя.
    let indices: Vec<u8> = (0..4).flat_map(|_| [1, 2, 2, 2]).collect();
    let cue = picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin: (1918, 1078),
        size: (4, 4),
        indices: &indices,
        palette: &[(1, color(255, 0, 0, 255)), (2, color(0, 0, 255, 255))],
        forced: PictureForcedFlag::Regular,
    });
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(1920, 1080),
    );
    let picture = only_picture(&overlay);
    assert_eq!(picture.origin(), PixelPoint { x: 1918, y: 1078 });
    assert_eq!((picture.width(), picture.height()), (2, 2));
    assert_eq!(texel(picture, 0, 1), [255, 0, 0, 255]);
    assert_eq!(texel(picture, 1, 1), [0, 0, 255, 255]);
    let bounds = overlay.content_bounds().expect("picture drawn");
    assert_eq!(
        (
            bounds.x + bounds.width as i32,
            bounds.y + bounds.height as i32
        ),
        (1920, 1080)
    );
}

#[test]
fn picture_entirely_outside_the_canvas_draws_nothing_and_is_not_an_error() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    let cue = solid((1920, 100), (4, 4), color(255, 0, 0, 255));
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(1920, 1080),
    );
    assert!(overlay.is_empty());
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.failed_pictures, 0);
    assert_eq!(statistics.rejected_pictures_too_large, 0);
}

#[test]
fn oversized_reference_is_rejected_and_counted_while_other_cues_are_drawn() {
    let mut rasterizer = rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_canvas_side: 2000,
            ..ROOMY_INPUT
        },
    });
    let test_cues = keys(2);
    let huge = picture_cue(&PictureSpec {
        reference: (4000, 2000),
        origin: (0, 1500),
        size: (2, 2),
        indices: &[1, 1, 1, 1],
        palette: &[(1, color(255, 0, 0, 255))],
        forced: PictureForcedFlag::Regular,
    });
    let normal = solid((100, 900), (2, 2), color(0, 255, 0, 255));
    let overlay = render_cues(
        &mut rasterizer,
        &[
            RenderableCue::picture(test_cues[0].key, &huge),
            RenderableCue::picture(test_cues[1].key, &normal),
        ],
        canvas_of(1920, 1080),
    );
    let picture = only_picture(&overlay);
    assert_eq!(texel(picture, 0, 0), [0, 255, 0, 255]);
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.rejected_pictures_too_large, 1);
    assert_eq!(statistics.failed_cues, 0);
}

#[test]
fn same_request_twice_is_unchanged_and_decodes_once() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    let cue = solid((100, 900), (6, 3), color(255, 0, 0, 255));
    let cues = [RenderableCue::picture(key, &cue)];
    let first = render_cues(&mut rasterizer, &cues, canvas_of(1280, 720));
    let second = render_cues(&mut rasterizer, &cues, canvas_of(1280, 720));

    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.revision(), first.revision());
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.rasterized_cues, 1);
    assert_eq!(statistics.unchanged_frames, 1);
    // Кадр держит ровно RGBA-пиксели картинки: 4 байта на пиксель.
    let picture = only_picture(&second);
    assert_eq!(
        statistics.current_frame_bytes,
        picture.width() as usize * picture.height() as usize * 4
    );
}

#[test]
fn picture_comes_from_the_cache_when_the_frame_changes_around_it_and_redraws_on_new_canvas() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = keys(2);
    let cue = solid((100, 900), (6, 3), color(255, 0, 0, 255));
    let other = solid((100, 100), (6, 3), color(0, 255, 0, 255));
    let first = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(test_cues[0].key, &cue)],
        canvas_of(1280, 720),
    );
    // Добавилась вторая картинка: кадр другой, но первая берётся из кэша тем же буфером.
    let second = render_cues(
        &mut rasterizer,
        &[
            RenderableCue::picture(test_cues[0].key, &cue),
            RenderableCue::picture(test_cues[1].key, &other),
        ],
        canvas_of(1280, 720),
    );
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
    assert!(
        only_first(&first).shows_same_pixels_as(&second.bitmaps()[0]),
        "cached picture must keep the same pixel buffer"
    );
    // Другой холст - другой ключ кэша: картинка пересчитана.
    render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(test_cues[0].key, &cue)],
        canvas_of(640, 360),
    );
    assert_eq!(rasterizer.statistics().rasterized_cues, 3);
}

fn only_first(overlay: &SubtitleOverlay) -> &SubtitleBitmap {
    &overlay.bitmaps()[0]
}

#[test]
fn bottom_pictures_belong_to_the_main_group_and_top_ones_to_the_fixed_group() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = keys(2);
    let bottom = solid((100, 900), (4, 4), color(255, 0, 0, 255));
    let top = solid((100, 100), (4, 4), color(0, 255, 0, 255));
    let overlay = render_cues(
        &mut rasterizer,
        &[
            RenderableCue::picture(test_cues[0].key, &bottom),
            RenderableCue::picture(test_cues[1].key, &top),
        ],
        canvas_of(1920, 1080),
    );
    assert_eq!(
        overlay.bitmaps()[0].placement_group(),
        OverlayPlacementGroup::Main
    );
    assert_eq!(
        overlay.bitmaps()[1].placement_group(),
        OverlayPlacementGroup::Fixed
    );
    let main = overlay
        .group_bounds(OverlayPlacementGroup::Main)
        .expect("main group");
    assert_eq!((main.x, main.y), (100, 900));
    let fixed = overlay
        .group_bounds(OverlayPlacementGroup::Fixed)
        .expect("fixed group");
    assert_eq!((fixed.x, fixed.y), (100, 100));
}

#[test]
fn pictures_are_drawn_after_text_and_forced_flag_stays_reachable() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = cues(7, &["Привет", "картинка"]);
    let forced = picture_cue(&PictureSpec {
        reference: (1280, 720),
        origin: (100, 600),
        size: (2, 2),
        indices: &[1, 1, 1, 1],
        palette: &[(1, color(255, 255, 0, 255))],
        forced: PictureForcedFlag::Forced,
    });
    // Картинка стоит в запросе ПЕРВОЙ, но рисуется поверх текста.
    let list = [
        RenderableCue::picture(test_cues[1].key, &forced),
        RenderableCue::text(test_cues[0].key, &test_cues[0].cue),
    ];
    let overlay = render_cues(&mut rasterizer, &list, canvas());

    let bitmaps = overlay.bitmaps();
    assert!(bitmaps.len() >= 2);
    let last = bitmaps.last().expect("non-empty");
    assert_eq!(last.kind(), SubtitleBitmapKind::Picture);
    assert!(
        bitmaps[..bitmaps.len() - 1]
            .iter()
            .all(|bitmap| bitmap.kind() != SubtitleBitmapKind::Picture)
    );
    assert_eq!(last.forced(), PictureForcedFlag::Forced);
    assert_eq!(bitmaps[0].forced(), PictureForcedFlag::Regular);
    // Владелец позволяет убрать картинку, когда её реплика закончилась.
    let without_picture = overlay.retain_cues(|key| key != test_cues[1].key);
    assert!(of_kind(&without_picture, SubtitleBitmapKind::Picture).is_empty());
    assert_eq!(without_picture.bitmaps().len(), bitmaps.len() - 1);
}

#[test]
fn composite_shows_picture_colours_at_their_canvas_position() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let key = keys(1)[0].key;
    let cue = picture_cue(&PictureSpec {
        reference: (1920, 1080),
        origin: (10, 20),
        size: (2, 1),
        indices: &[1, 2],
        palette: &[(1, color(255, 0, 0, 255)), (2, color(0, 0, 255, 128))],
        forced: PictureForcedFlag::Regular,
    });
    let overlay = render_cues(
        &mut rasterizer,
        &[RenderableCue::picture(key, &cue)],
        canvas_of(1920, 1080),
    );
    let pixels = overlay.composite_rgba_over_transparent();
    let at = |x: usize, y: usize| -> [u8; 4] {
        let offset = (y * 1920 + x) * 4;
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            pixels[offset + 3],
        ]
    };
    assert_eq!(at(10, 20), [255, 0, 0, 255]);
    // Полупрозрачный синий: прямой цвет восстановлен из премультиплицированного (допуск 1).
    let [red, green, blue, alpha] = at(11, 20);
    assert_eq!((red, green, alpha), (0, 0, 128));
    assert!(blue >= 254, "straight blue must be restored, got {blue}");
    assert_eq!(at(9, 20), [0, 0, 0, 0]);
    assert_eq!(at(10, 21), [0, 0, 0, 0]);
}

#[test]
fn pictures_over_the_frame_limit_are_dropped_and_counted() {
    let mut rasterizer = rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_cues_per_frame: 1,
            ..ROOMY_INPUT
        },
    });
    let test_cues = keys(2);
    let first = solid((100, 900), (2, 2), color(255, 0, 0, 255));
    let second = solid((300, 900), (2, 2), color(0, 255, 0, 255));
    let overlay = render_cues(
        &mut rasterizer,
        &[
            RenderableCue::picture(test_cues[0].key, &first),
            RenderableCue::picture(test_cues[1].key, &second),
        ],
        canvas_of(1920, 1080),
    );
    assert_eq!(
        only_picture(&overlay).origin(),
        PixelPoint { x: 100, y: 900 }
    );
    assert_eq!(rasterizer.statistics().dropped_pictures_over_frame_limit, 1);
}
