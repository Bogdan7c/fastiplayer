use subtitle_core::{SpanStyle, StyledText, SubtitleColor, TextSpan};

use super::*;
use crate::event_style::AxisPair;
use crate::overlay::{
    CanvasSize, OverlayChange, OverlayRevision, SubtitleBitmapKind, SubtitleOverlay,
};
use crate::test_support::{embedded_fonts, plain_style, plain_text};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn canvas() -> CanvasSize {
    CanvasSize::new(WIDTH, HEIGHT).expect("canvas")
}

/// Рамка закрашенных пикселей всех картинок: `(x0, y0, x1, y1)`.
fn ink_bounds(images: &[SubtitleBitmap]) -> (i32, i32, i32, i32) {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for image in images {
        for row in 0..image.height() {
            let cells = image.coverage_row(row).expect("row");
            for (column, value) in cells.iter().take(image.width() as usize).enumerate() {
                if *value != 0 {
                    let (x, y) = (
                        image.origin().x + column as i32,
                        image.origin().y + row as i32,
                    );
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
    }
    (x0, y0, x1, y1)
}

#[test]
fn russian_phrase_is_inside_the_canvas_centered_and_sits_on_the_bottom_margin() {
    let mut style = plain_style();
    style.border = AxisPair::uniform(2.0);
    let images = render_text_event(
        &mut embedded_fonts(),
        &plain_text("Привет, как дела?"),
        &style,
        canvas(),
    )
    .expect("render");
    assert!(!images.is_empty());
    // Контракт выхода: все картинки внутри холста (иначе `new` вернёт ошибку).
    let overlay = SubtitleOverlay::new(
        canvas(),
        images.clone(),
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .expect("images fit the canvas");
    assert!(overlay.bitmaps().iter().all(|b| b.width() > 0));

    let fills: Vec<_> = images
        .iter()
        .filter(|i| i.kind() == SubtitleBitmapKind::Glyph)
        .cloned()
        .collect();
    let (x0, _, x1, y1) = ink_bounds(&fills);
    let center = f64::from(x0 + x1) / 2.0;
    assert!(
        (center - f64::from(WIDTH) / 2.0).abs() <= 2.0,
        "center {center}"
    );
    // Низ текста (запятая уходит ниже базовой линии) — у нижнего поля 40 px.
    assert!((680 - 16..=680).contains(&y1), "bottom {y1}");
}

#[test]
fn empty_and_blank_text_give_no_images() {
    let mut fonts = embedded_fonts();
    let style = plain_style();
    for text in [
        StyledText::new(vec![]),
        plain_text(""),
        plain_text("  \t "),
        plain_text("\n"),
    ] {
        assert!(
            render_text_event(&mut fonts, &text, &style, canvas())
                .expect("render")
                .is_empty()
        );
    }
}

#[test]
fn symbol_missing_from_every_font_is_drawn_as_notdef_and_counted() {
    let mut fonts = embedded_fonts();
    let style = plain_style();
    let before = fonts.statistics().missing_glyph_lookups;
    let with_missing =
        render_text_event(&mut fonts, &plain_text("a\u{E000}b"), &style, canvas()).expect("render");
    assert!(
        fonts.statistics().missing_glyph_lookups > before,
        "counter grows"
    );
    let without =
        render_text_event(&mut fonts, &plain_text("ab"), &style, canvas()).expect("render");
    let (a0, _, a1, _) = ink_bounds(&with_missing);
    let (b0, _, b1, _) = ink_bounds(&without);
    assert!(
        a1 - a0 > b1 - b0,
        "the notdef box takes space between the letters"
    );
}

#[test]
fn author_color_replaces_the_fill_color_of_its_span_only() {
    let red = SubtitleColor {
        red: 255,
        green: 0,
        blue: 0,
        alpha: 255,
    };
    let text = StyledText::new(vec![
        TextSpan::text("ab", SpanStyle::default()),
        TextSpan::text(
            "cd",
            SpanStyle {
                color: Some(red),
                ..SpanStyle::default()
            },
        ),
    ]);
    let images =
        render_text_event(&mut embedded_fonts(), &text, &plain_style(), canvas()).expect("render");
    let colors: Vec<_> = images
        .iter()
        .filter(|i| i.kind() == SubtitleBitmapKind::Glyph)
        .map(|i| i.color())
        .collect();
    assert_eq!(
        colors,
        vec![crate::test_support::WHITE, red],
        "one white run and one red run"
    );
}

#[test]
fn invalid_style_numbers_are_typed_errors() {
    let mut fonts = embedded_fonts();
    let mut style = plain_style();
    style.font_size = 0.0;
    assert_eq!(
        render_text_event(&mut fonts, &plain_text("a"), &style, canvas()).err(),
        Some(TextRenderError::InvalidFontSize { size: 0.0 })
    );
    style.font_size = 48.0;
    style.border = AxisPair::uniform(f64::NAN);
    assert_eq!(
        render_text_event(&mut fonts, &plain_text("a"), &style, canvas()).err(),
        Some(TextRenderError::InvalidDecorationSize)
    );
}

#[test]
fn unbuildable_border_is_counted_and_the_fill_is_still_drawn() {
    let mut faces = FaceCache::new();
    let mut absurd = plain_style();
    absurd.border = AxisPair::uniform(1.0e7);
    let broken = render_cue_text(
        &mut embedded_fonts(),
        &mut faces,
        &plain_text("Hi"),
        &absurd,
        canvas(),
        crate::fonts::TextLanguagePreference::unknown(),
    )
    .expect("the cue is rendered without borders");
    assert_eq!(
        broken.diagnostics.skipped_borders, 2,
        "both letters lose the border"
    );
    assert_eq!(broken.diagnostics.skipped_glyphs, 0);
    assert!(
        broken
            .images
            .iter()
            .all(|i| i.kind() == SubtitleBitmapKind::Glyph),
        "only fills remain"
    );
    assert!(!broken.images.is_empty());

    // Обычная обводка ничего не пропускает и рисуется.
    let mut normal = plain_style();
    normal.border = AxisPair::uniform(2.0);
    let fine = render_cue_text(
        &mut embedded_fonts(),
        &mut faces,
        &plain_text("Hi"),
        &normal,
        canvas(),
        crate::fonts::TextLanguagePreference::unknown(),
    )
    .expect("render");
    assert_eq!(fine.diagnostics, RenderDiagnostics::default());
    assert!(
        fine.images
            .iter()
            .any(|i| i.kind() == SubtitleBitmapKind::Outline)
    );
}
