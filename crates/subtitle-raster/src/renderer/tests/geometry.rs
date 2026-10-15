//! Геометрия и слои: где стоит текст, плашка, обводка и тень.

use subtitle_core::SubtitleColor;

use super::*;
use crate::overlay::{OverlayChange, OverlayRevision};

/// Плашка и глифы реплики «Привет, как дела?» в стиле YouTube на кадре 1280x720.
fn russian_youtube() -> SubtitleOverlay {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_once(
        &mut rasterizer,
        &cues(1, &["Привет, как дела?"]),
        &SubtitleAppearance::youtube_like(),
    )
}

#[test]
fn youtube_cue_has_background_first_and_text_centered_at_the_bottom() {
    let overlay = russian_youtube();
    assert!(!overlay.is_empty());
    assert_eq!(overlay.bitmaps()[0].kind(), SubtitleBitmapKind::Background);
    assert!(
        overlay.bitmaps()[1..]
            .iter()
            .all(|bitmap| bitmap.kind() == SubtitleBitmapKind::Glyph),
        "youtube style draws no outline and no shadow"
    );

    let glyphs = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Glyph)).expect("glyph ink");
    let center = f64::from(glyphs.0 + glyphs.2) / 2.0;
    assert!((center - 640.0).abs() <= 2.0, "text center {center}");

    // Нижний край плашки стоит на высоте 720 * (1 - 0.06) = 676,8.
    let plate = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Background)).expect("plate ink");
    assert!(
        (f64::from(plate.3) - 676.8).abs() <= 1.5,
        "plate bottom {}",
        plate.3
    );
}

#[test]
fn plate_is_wider_than_the_glyph_ink_by_exactly_the_configured_padding() {
    let overlay = russian_youtube();
    let glyphs = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Glyph)).expect("glyph ink");
    let plate = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Background)).expect("plate ink");
    // Отступ 0,25 em при ячейке 40 px = 10 px с каждой стороны.
    let left_gap = glyphs.0 - plate.0;
    let right_gap = plate.2 - glyphs.2;
    assert!((left_gap - 10).abs() <= 1, "left gap {left_gap}");
    assert!((right_gap - 10).abs() <= 1, "right gap {right_gap}");
    // Вертикально плашка охватывает буквы с запасом.
    assert!(plate.1 < glyphs.1 && plate.3 > glyphs.3);
}

#[test]
fn plate_corners_are_rounded_and_side_middles_are_solid() {
    let overlay = russian_youtube();
    let plate_bitmap = of_kind(&overlay, SubtitleBitmapKind::Background)[0];
    let (x0, y0, x1, y1) = ink_bounds([plate_bitmap]).expect("plate ink");
    let middle_y = (y0 + y1) / 2;
    let middle_x = (x0 + x1) / 2;
    // Скругление радиусом 6 px срезает угловой пиксель.
    assert!(
        coverage_at(plate_bitmap, x0, y0) < 255,
        "top-left corner not rounded"
    );
    assert!(
        coverage_at(plate_bitmap, x1 - 1, y1 - 1) < 255,
        "bottom-right corner not rounded"
    );
    assert_eq!(coverage_at(plate_bitmap, x0 + 1, middle_y), 255);
    assert_eq!(coverage_at(plate_bitmap, x1 - 2, middle_y), 255);
    assert_eq!(coverage_at(plate_bitmap, middle_x, y0 + 1), 255);
    assert_eq!(coverage_at(plate_bitmap, middle_x, y1 - 2), 255);
}

#[test]
fn two_lines_have_one_plate_whose_junction_is_not_darker_than_a_single_line() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = SubtitleAppearance::youtube_like();
    let overlay = render_once(
        &mut rasterizer,
        &cues(1, &["Первая строка\nВторая"]),
        &appearance,
    );

    let plates = of_kind(&overlay, SubtitleBitmapKind::Background);
    assert_eq!(plates.len(), 1, "all lines share one plate mask");
    let plate = plates[0];
    let lonely_plate = SubtitleOverlay::new(
        canvas(),
        vec![plate.clone()],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .expect("overlay");
    let rgba = lonely_plate.composite_rgba_over_transparent();
    let alpha_at = |x: i32, y: i32| rgba[((y as usize) * WIDTH as usize + x as usize) * 4 + 3];

    let (x0, y0, x1, y1) = ink_bounds([plate]).expect("plate ink");
    // Плашка держит ту же прозрачность 191/255 (0,75) везде: и на строках, и на стыке.
    let single_line_alpha = alpha_at((x0 + x1) / 2, y0 + 5);
    assert_eq!(single_line_alpha, 191);
    let mut alphas_in_column = Vec::new();
    for y in (y0 + 4)..(y1 - 4) {
        alphas_in_column.push(alpha_at((x0 + x1) / 2, y));
    }
    assert!(
        alphas_in_column.iter().all(|alpha| *alpha == 191),
        "junction of lines must not be darker or lighter: {alphas_in_column:?}"
    );
    // Две строки: плашка выше, чем для одной.
    let single = render_once(&mut rasterizer, &cues(2, &["Первая строка"]), &appearance);
    let single_plate = ink_bounds(of_kind(&single, SubtitleBitmapKind::Background)).expect("ink");
    assert!(y1 - y0 > (single_plate.3 - single_plate.1) * 3 / 2);
}

#[test]
fn outline_and_shadow_style_has_shadow_outline_and_glyph_layers_but_no_background() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_once(
        &mut rasterizer,
        &cues(1, &["Привет, как дела?"]),
        &SubtitleAppearance::outline_and_shadow(),
    );
    assert!(of_kind(&overlay, SubtitleBitmapKind::Background).is_empty());
    let kinds: Vec<_> = overlay.bitmaps().iter().map(SubtitleBitmap::kind).collect();
    let position = |kind| kinds.iter().position(|candidate| *candidate == kind);
    let shadow = position(SubtitleBitmapKind::Shadow).expect("shadow layer");
    let outline = position(SubtitleBitmapKind::Outline).expect("outline layer");
    let glyph = position(SubtitleBitmapKind::Glyph).expect("glyph layer");
    assert!(shadow < outline && outline < glyph, "draw order {kinds:?}");
    // Цвета слоёв: чёрная обводка, белые буквы, тень полупрозрачная.
    assert_eq!(
        overlay.bitmaps()[outline].color(),
        SubtitleColor::opaque(0, 0, 0)
    );
    assert_eq!(
        overlay.bitmaps()[glyph].color(),
        SubtitleColor::opaque(255, 255, 255)
    );
    assert_eq!(overlay.bitmaps()[shadow].color().alpha, 153);
}

#[test]
fn author_color_overrides_the_user_text_color() {
    use subtitle_core::{SpanStyle, TextSpan};
    let red = SubtitleColor::opaque(255, 0, 0);
    let mut test_cues = cues(1, &["x"]);
    test_cues[0].cue.text = StyledText::new(vec![
        TextSpan::text(
            "Красный ",
            SpanStyle {
                color: Some(red),
                ..SpanStyle::default()
            },
        ),
        TextSpan::text("белый", SpanStyle::default()),
    ]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_once(
        &mut rasterizer,
        &test_cues,
        &SubtitleAppearance::youtube_like(),
    );
    let colors: Vec<_> = of_kind(&overlay, SubtitleBitmapKind::Glyph)
        .iter()
        .map(|bitmap| bitmap.color())
        .collect();
    assert!(colors.contains(&red), "{colors:?}");
    assert!(
        colors.contains(&SubtitleColor::opaque(255, 255, 255)),
        "{colors:?}"
    );
}

#[test]
fn two_simultaneous_bottom_cues_do_not_overlap_and_first_stays_in_place() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = SubtitleAppearance::youtube_like();
    let alone = render_once(&mut rasterizer, &cues(1, &["Нижняя реплика"]), &appearance);
    let alone_plate = ink_bounds(of_kind(&alone, SubtitleBitmapKind::Background)).expect("ink");

    let both = cues(2, &["Нижняя реплика", "Вторая реплика"]);
    let overlay = render_once(&mut rasterizer, &both, &appearance);
    let plates = of_kind(&overlay, SubtitleBitmapKind::Background);
    assert_eq!(plates.len(), 2);
    let first = ink_bounds([plates[0]]).expect("first plate");
    let second = ink_bounds([plates[1]]).expect("second plate");
    assert_eq!(first, alone_plate, "the first cue must not move");
    assert!(
        second.3 <= first.1,
        "second plate {second:?} must sit above the first {first:?}"
    );
    // Текст второй реплики поднялся вместе с её плашкой.
    let glyph_groups: Vec<_> = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    let upper_ink = ink_bounds(
        glyph_groups
            .iter()
            .copied()
            .filter(|bitmap| bitmap.origin().y < first.1),
    )
    .expect("upper glyphs");
    assert!(upper_ink.3 <= first.1 && upper_ink.1 >= second.1);
}

#[test]
fn top_anchored_cues_stack_downwards() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = SubtitleAppearance::youtube_like();
    let mut test_cues = cues(1, &["Верхняя первая", "Верхняя вторая"]);
    for test_cue in &mut test_cues {
        test_cue.cue.placement.anchor = Some(subtitle_core::ScreenAnchor::TopCenter);
    }
    let overlay = render_once(&mut rasterizer, &test_cues, &appearance);
    let plates = of_kind(&overlay, SubtitleBitmapKind::Background);
    let first = ink_bounds([plates[0]]).expect("first");
    let second = ink_bounds([plates[1]]).expect("second");
    assert!(first.1 < 100, "anchored near the top: {first:?}");
    assert!(
        second.1 >= first.3,
        "second below the first: {second:?} vs {first:?}"
    );
}
