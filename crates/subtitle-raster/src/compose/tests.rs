use subtitle_core::SubtitleColor;

use crate::event_style::{AxisPair, BorderStyle};
use crate::overlay::{CanvasSize, SubtitleBitmap, SubtitleBitmapKind};
use crate::test_support::{WHITE, embedded_fonts, plain_style, plain_text};
use crate::text_renderer::render_text_event;

fn render(
    text: &str,
    tweak: impl FnOnce(&mut crate::event_style::EventStyle),
) -> Vec<SubtitleBitmap> {
    let mut style = plain_style();
    tweak(&mut style);
    let canvas = CanvasSize::new(1280, 720).expect("canvas");
    render_text_event(&mut embedded_fonts(), &plain_text(text), &style, canvas).expect("render")
}

fn kinds(images: &[SubtitleBitmap]) -> Vec<SubtitleBitmapKind> {
    images.iter().map(SubtitleBitmap::kind).collect()
}

fn ink(image: &SubtitleBitmap) -> u64 {
    image.coverage().iter().map(|v| u64::from(*v)).sum()
}

/// Рамка закрашенных пикселей в координатах холста: `(x0, y0, x1, y1)`, правая и нижняя — исключая.
fn ink_bounds(image: &SubtitleBitmap) -> (i32, i32, i32, i32) {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
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
    (x0, y0, x1, y1)
}

#[test]
fn plain_style_produces_only_a_fill_layer() {
    let images = render("Hello", |_| {});
    assert_eq!(kinds(&images), vec![SubtitleBitmapKind::Glyph]);
    assert_eq!(images[0].color(), WHITE);
    assert!(ink(&images[0]) > 0);
}

#[test]
fn outline_adds_a_layer_under_the_fill_in_libass_order() {
    let images = render("Hello", |style| style.border = AxisPair::uniform(3.0));
    assert_eq!(
        kinds(&images),
        vec![SubtitleBitmapKind::Outline, SubtitleBitmapKind::Glyph]
    );
    let (outline, fill) = (ink_bounds(&images[0]), ink_bounds(&images[1]));
    assert_eq!(
        (fill.0 - outline.0, fill.1 - outline.1),
        (3, 3),
        "border grows by 3 px on each side"
    );
    assert_eq!((outline.2 - fill.2, outline.3 - fill.3), (3, 3));
}

#[test]
fn shadow_layer_comes_first_and_is_shifted_by_the_offset() {
    let images = render("Hello", |style| {
        style.border = AxisPair::uniform(2.0);
        style.shadow = AxisPair::uniform(4.0);
    });
    assert_eq!(
        kinds(&images),
        vec![
            SubtitleBitmapKind::Shadow,
            SubtitleBitmapKind::Outline,
            SubtitleBitmapKind::Glyph
        ]
    );
    let (shadow, outline) = (&images[0], &images[1]);
    assert_eq!(shadow.origin().x - outline.origin().x, 4);
    assert_eq!(shadow.origin().y - outline.origin().y, 4);
    assert_eq!(shadow.color(), plain_style().colors.back);
}

#[test]
fn shadow_without_border_follows_the_fill_visibility_like_libass() {
    // Непрозрачная заливка: тень рисуется (libass: FILL_IN_SHADOW).
    let images = render("Hello", |style| style.shadow = AxisPair::uniform(4.0));
    assert_eq!(
        kinds(&images),
        vec![SubtitleBitmapKind::Shadow, SubtitleBitmapKind::Glyph]
    );
    // Невидимая заливка без обводки: ни тени, ни букв.
    let invisible = render("Hello", |style| {
        style.shadow = AxisPair::uniform(4.0);
        style.colors.primary = SubtitleColor {
            alpha: 0,
            ..style.colors.primary
        };
    });
    assert!(invisible.is_empty());
}

#[test]
fn event_background_comes_first_covers_the_text_and_replaces_shadows() {
    let images = render("Hello", |style| {
        style.border_style = BorderStyle::EventBackground;
        style.border = AxisPair::uniform(2.0);
        style.shadow = AxisPair::uniform(3.0);
    });
    assert_eq!(
        kinds(&images),
        vec![
            SubtitleBitmapKind::Background,
            SubtitleBitmapKind::Outline,
            SubtitleBitmapKind::Glyph
        ]
    );
    let background = &images[0];
    let (outline, fill) = (ink_bounds(&images[1]), ink_bounds(&images[2]));
    let b = ink_bounds(background);
    // Подложка охватывает весь видимый текст и обводку.
    assert!(
        b.0 <= outline.0 && b.1 <= outline.1 && b.2 >= outline.2 && b.3 >= outline.3,
        "{b:?} vs {outline:?}"
    );
    assert!(b.0 <= fill.0 && b.2 >= fill.2);
    assert!(background.coverage().iter().all(|v| *v == 255));
}

#[test]
fn opaque_box_draws_outline_color_boxes_and_a_shadow() {
    let images = render("Hi", |style| {
        style.border_style = BorderStyle::OpaqueBox;
        style.border = AxisPair::uniform(2.0);
        style.shadow = AxisPair::uniform(2.0);
    });
    assert_eq!(
        kinds(&images),
        vec![
            SubtitleBitmapKind::Shadow,
            SubtitleBitmapKind::Outline,
            SubtitleBitmapKind::Glyph
        ]
    );
    // Плашка закрывает всю ширину текста сплошным покрытием.
    let outline = &images[1];
    assert!(
        outline
            .coverage_row(outline.height() / 2)
            .expect("row")
            .contains(&255)
    );
}

#[test]
fn transparent_colors_are_not_emitted() {
    let images = render("Hello", |style| {
        style.border = AxisPair::uniform(2.0);
        style.colors.outline = SubtitleColor {
            alpha: 0,
            ..style.colors.outline
        };
    });
    assert_eq!(kinds(&images), vec![SubtitleBitmapKind::Glyph]);
}

#[test]
fn text_partly_outside_the_canvas_is_cropped_not_dropped() {
    let images = render("Hello world", |style| {
        style.margins.left = 0;
        style.margins.right = 0;
        // Рамка 20 px: слева текст упирается в край кадра, обводка выходит за него.
        style.border = AxisPair::uniform(20.0);
        style.alignment = subtitle_core::ScreenAnchor::BottomLeft;
    });
    let outline = images
        .iter()
        .find(|i| i.kind() == SubtitleBitmapKind::Outline)
        .expect("outline");
    assert_eq!(outline.origin().x, 0, "cropped at the canvas edge");
    assert!(outline.width() > 100);
    assert!(ink(outline) > 0);
}

/// Слой 8x8 с одним ярким пикселем в центре: после гауссиана покрытие расплывается.
fn bright_dot_layer() -> crate::bitmap::CoverageBitmap {
    let mut layer = crate::bitmap::CoverageBitmap::zeroed(16, 16).expect("layer");
    layer.buffer[8 * layer.stride + 8] = 255;
    layer
}

fn gaussian_strength() -> crate::blur::BlurStrength {
    crate::blur::BlurStrength {
        gaussian: crate::blur::GaussianVariance { x: 4.0, y: 4.0 },
        box_passes: crate::blur::BoxBlurPasses::clamped(0),
    }
}

#[test]
fn blur_layer_reports_blurred_oversized_and_failed_layers_separately() {
    use super::run::{BlurEnv, LayerBlur, blur_layer};
    use crate::blur::{BitmapSideLimit, BlurScratch};

    let mut scratch = BlurScratch::new();

    // Штатный слой размывается: пиксель расплылся.
    let mut blurred = bright_dot_layer();
    let original = blurred.clone();
    let mut env = BlurEnv {
        scratch: &mut scratch,
        side_limit: BitmapSideLimit::UNLIMITED,
    };
    assert_eq!(
        blur_layer(Some(&mut blurred), gaussian_strength(), &mut env),
        LayerBlur::Done
    );
    assert_ne!(blurred, original);

    // Слоя нет: размывать нечего, это не ошибка.
    assert_eq!(
        blur_layer(None, gaussian_strength(), &mut env),
        LayerBlur::Done
    );

    // Результат не влезает в предел стороны: штатный пропуск, слой цел.
    let mut too_wide = bright_dot_layer();
    let mut tight = BlurEnv {
        scratch: env.scratch,
        side_limit: BitmapSideLimit(16),
    };
    assert_eq!(
        blur_layer(Some(&mut too_wide), gaussian_strength(), &mut tight),
        LayerBlur::SkippedOversized
    );
    assert_eq!(too_wide, original);

    // Буфер короче, чем заявляет геометрия: нарушен инвариант, считается отдельно.
    let mut inconsistent = bright_dot_layer();
    inconsistent.buffer.truncate(8);
    let broken_before = inconsistent.clone();
    let mut env = BlurEnv {
        scratch: tight.scratch,
        side_limit: BitmapSideLimit::UNLIMITED,
    };
    assert_eq!(
        blur_layer(Some(&mut inconsistent), gaussian_strength(), &mut env),
        LayerBlur::Failed
    );
    assert_eq!(inconsistent, broken_before);
}
