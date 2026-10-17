//! Размытие ASS (`\blur`, `\be`) через публичный `SubtitleRasterizer::render`: файл `.ass` на
//! входе, картинки на холсте на выходе. Проверяется то, что видно на экране: край стал мягче,
//! сумма покрытия сохранилась, размывается именно обводка, слишком большое размытие не роняет
//! реплику, а статичная размытая реплика рисуется один раз.

use super::ass::{glyph_ink, render_ass, render_ass_with, single};
use super::*;
use crate::ass::AssRenderOptions;

/// Сумма покрытия всех картинок вида `kind`: «количество чернил».
fn total_coverage(overlay: &SubtitleOverlay, kind: SubtitleBitmapKind) -> u64 {
    of_kind(overlay, kind)
        .into_iter()
        .flat_map(|bitmap| (0..bitmap.height()).filter_map(move |row| bitmap.coverage_row(row)))
        .flat_map(|row| row.iter().map(|value| u64::from(*value)))
        .sum()
}

/// Сколько пикселей вида `kind` закрашено частично (мягкий край).
fn partial_pixels(overlay: &SubtitleOverlay, kind: SubtitleBitmapKind) -> usize {
    of_kind(overlay, kind)
        .into_iter()
        .flat_map(|bitmap| (0..bitmap.height()).filter_map(move |row| bitmap.coverage_row(row)))
        .flat_map(|row| row.iter().copied())
        .filter(|value| (1..255).contains(value))
        .count()
}

fn draw(tags: &str, text: &str) -> SubtitleOverlay {
    render_ass(
        &mut rasterizer(ROOMY_LIMITS),
        &single(&format!(r"{{\an7\pos(200,200){tags}}}{text}")),
    )
}

/// Все слои вида `kind` как `(x, y, ширина, высота, покрытие)`: для сравнения «слой не изменился».
fn layer_snapshot(
    overlay: &SubtitleOverlay,
    kind: SubtitleBitmapKind,
) -> Vec<(i32, i32, u32, u32, Vec<u8>)> {
    of_kind(overlay, kind)
        .into_iter()
        .map(|bitmap| {
            let rows: Vec<u8> = (0..bitmap.height())
                .filter_map(|row| bitmap.coverage_row(row))
                .flat_map(|row| row.iter().copied())
                .collect();
            (
                bitmap.origin().x,
                bitmap.origin().y,
                bitmap.width(),
                bitmap.height(),
                rows,
            )
        })
        .collect()
}

#[test]
fn blur_softens_the_edge_and_keeps_the_total_ink() {
    let kind = SubtitleBitmapKind::Glyph;
    let sharp = draw(r"\bord0\fs80", "H");
    let soft = draw(r"\bord0\fs80\blur4", "H");

    // Мягкий край: частично закрашенных пикселей заметно больше, картинка шире.
    assert!(
        partial_pixels(&soft, kind) > 3 * partial_pixels(&sharp, kind),
        "{} против {}",
        partial_pixels(&soft, kind),
        partial_pixels(&sharp, kind)
    );
    let (sharp_ink, soft_ink) = (glyph_ink(&sharp), glyph_ink(&soft));
    assert!(
        soft_ink.0 < sharp_ink.0 && soft_ink.2 > sharp_ink.2,
        "{soft_ink:?}"
    );
    assert!(
        soft_ink.1 < sharp_ink.1 && soft_ink.3 > sharp_ink.3,
        "{soft_ink:?}"
    );

    // Размытие перераспределяет чернила, а не создаёт и не съедает их (допуск 3 %).
    let (before, after) = (total_coverage(&sharp, kind), total_coverage(&soft, kind));
    assert!(before > 0);
    assert!(
        before.abs_diff(after) * 100 < before * 3,
        "{before} -> {after}"
    );
}

#[test]
fn zero_blur_is_the_same_picture_as_no_blur_tag() {
    let plain = draw(r"\bord2", "Hi");
    let zero = draw(r"\bord2\blur0\be0", "Hi");
    for kind in [
        SubtitleBitmapKind::Glyph,
        SubtitleBitmapKind::Outline,
        SubtitleBitmapKind::Shadow,
    ] {
        assert_eq!(layer_snapshot(&plain, kind), layer_snapshot(&zero, kind));
    }
}

#[test]
fn with_an_outline_only_the_outline_layer_is_blurred() {
    let sharp = draw(r"\bord4\fs80\blur0", "H");
    let soft = draw(r"\bord4\fs80\blur5", "H");
    // Заливка при обводке не размывается (libass: `FILTER_NONZERO_BORDER`) ...
    assert_eq!(
        layer_snapshot(&sharp, SubtitleBitmapKind::Glyph),
        layer_snapshot(&soft, SubtitleBitmapKind::Glyph)
    );
    // ... а обводка становится мягче и выходит за прежние границы.
    assert!(
        partial_pixels(&soft, SubtitleBitmapKind::Outline)
            > 2 * partial_pixels(&sharp, SubtitleBitmapKind::Outline)
    );
    let outline_ink = |overlay: &SubtitleOverlay| {
        ink_bounds(of_kind(overlay, SubtitleBitmapKind::Outline)).expect("outline ink")
    };
    assert!(outline_ink(&soft).0 < outline_ink(&sharp).0);
}

#[test]
fn shadow_is_copied_from_the_blurred_layer() {
    let sharp = draw(r"\bord0\shad6\fs80\blur0", "H");
    let soft = draw(r"\bord0\shad6\fs80\blur5", "H");
    let kind = SubtitleBitmapKind::Shadow;
    assert!(partial_pixels(&soft, kind) > 3 * partial_pixels(&sharp, kind));
    // Тень не стала резче заливки: её край мягкий вместе с заливкой.
    assert!(total_coverage(&soft, kind) > 0);
}

#[test]
fn edge_blur_grows_the_picture_and_softens_it() {
    let kind = SubtitleBitmapKind::Glyph;
    let sharp = draw(r"\bord0\fs80", "H");
    let edge = draw(r"\bord0\fs80\be3", "H");
    assert!(partial_pixels(&edge, kind) > 2 * partial_pixels(&sharp, kind));
    let (before, after) = (total_coverage(&sharp, kind), total_coverage(&edge, kind));
    assert!(
        before.abs_diff(after) * 100 < before * 5,
        "{before} -> {after}"
    );
}

#[test]
fn blur_runs_split_the_line_and_each_part_keeps_its_own_softness() {
    // Слева резкая «H», справа размытая: это два участка, у второго край мягче.
    let overlay = draw(r"\bord0\fs80", r"H{\blur6}H");
    let glyphs = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    assert_eq!(glyphs.len(), 2, "размытие меняет участок: две картинки");
    let sharp = glyphs[0];
    let soft = glyphs[1];
    assert!(
        soft.width() > sharp.width() + 8,
        "{} / {}",
        soft.width(),
        sharp.width()
    );
}

#[test]
fn oversized_blur_keeps_the_cue_visible_and_is_counted() {
    // Холст 1280x720, предел стороны 1280: огромное размытие огромных букв не помещается.
    let cues = single(r"{\an5\pos(640,360)\bord0\fs250\blur100}HHHHHH");
    let mut limited = rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_canvas_side: 1_280,
            ..ROOMY_INPUT
        },
    });
    let overlay = render_ass(&mut limited, &cues);
    assert!(limited.statistics().skipped_oversized_bitmaps > 0);
    // Реплика не пропала: слой остался неразмытым (как libass при нехватке памяти).
    assert!(total_coverage(&overlay, SubtitleBitmapKind::Glyph) > 0);

    // Контроль: умеренное размытие тех же букв ничего не пропускает.
    let mut control = rasterizer(ROOMY_LIMITS);
    render_ass(
        &mut control,
        &single(r"{\an5\pos(640,360)\bord0\fs250\blur3}HHHHHH"),
    );
    assert_eq!(control.statistics().skipped_oversized_bitmaps, 0);
}

#[test]
fn edge_blur_still_works_when_the_gaussian_step_does_not_fit() {
    // libass не проверяет результат гауссиана: если он отказал, `\be` всё равно применяется.
    let limits = RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_canvas_side: 1_280,
            ..ROOMY_INPUT
        },
    };
    let draw_limited = |tags: &str| {
        let mut limited = rasterizer_with_limits(limits);
        let overlay = render_ass(
            &mut limited,
            &single(&format!(r"{{\an5\pos(640,360)\bord0\fs250{tags}}}HHHHHH")),
        );
        (overlay, limited.statistics().skipped_oversized_bitmaps)
    };
    let (gaussian_only, skipped_plain) = draw_limited(r"\blur100");
    let (with_edge, skipped_with_edge) = draw_limited(r"\blur100\be4");
    // Гауссиан пропущен в обоих случаях (и посчитан), поэтому первая картинка осталась резкой.
    assert_eq!((skipped_plain, skipped_with_edge), (1, 1));
    let kind = SubtitleBitmapKind::Glyph;
    assert!(
        partial_pixels(&with_edge, kind) > 2 * partial_pixels(&gaussian_only, kind),
        "{} против {}",
        partial_pixels(&with_edge, kind),
        partial_pixels(&gaussian_only, kind)
    );
}

#[test]
fn user_style_mode_ignores_author_blur() {
    let cues = single(r"{\an7\pos(200,200)\bord0\fs80\blur8}H");
    let options = AssRenderOptions {
        style_source: crate::ass::AssStyleSource::User,
        ..AssRenderOptions::default()
    };
    let appearance = SubtitleAppearance::youtube_like();
    let user = render_ass_with(
        &mut rasterizer(ROOMY_LIMITS),
        &cues,
        options,
        &appearance,
        canvas(),
    );
    let author = render_ass(&mut rasterizer(ROOMY_LIMITS), &cues);
    assert!(!user.is_empty());
    assert!(
        partial_pixels(&author, SubtitleBitmapKind::Glyph)
            > 2 * partial_pixels(&user, SubtitleBitmapKind::Glyph)
    );
}

#[test]
fn static_blurred_cue_is_rasterized_once_across_frames() {
    let file_cues = super::ass::single(r"{\an7\pos(200,200)\bord2\blur3}Cached");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_ass(&mut rasterizer, &file_cues);
    let rasterized = rasterizer.statistics().rasterized_cues;
    assert_eq!(rasterized, 1);
    // Другой холст-запрос с теми же репликами и опциями кэш реплик не трогает.
    let second = render_ass(&mut rasterizer, &file_cues);
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(rasterizer.statistics().rasterized_cues, rasterized);
    assert_eq!(
        layer_snapshot(&first, SubtitleBitmapKind::Outline),
        layer_snapshot(&second, SubtitleBitmapKind::Outline)
    );
}

#[test]
fn blur_at_the_canvas_edge_is_cropped_inside_the_canvas() {
    // Размытие выводит картинку за левый верхний угол холста: обрезка обязана сохранить
    // видимую часть и инвариант «картинка целиком внутри холста».
    let overlay = render_ass(
        &mut rasterizer(ROOMY_LIMITS),
        &single(r"{\an7\pos(0,0)\bord3\fs80\blur10}HH"),
    );
    assert!(total_coverage(&overlay, SubtitleBitmapKind::Outline) > 0);
    for bitmap in overlay.bitmaps() {
        let origin = bitmap.origin();
        assert!(origin.x >= 0 && origin.y >= 0, "{origin:?}");
        assert!(origin.x + bitmap.width() as i32 <= WIDTH as i32);
        assert!(origin.y + bitmap.height() as i32 <= HEIGHT as i32);
    }
}

/// Рендер большого размытия и объём плоскостей, которые растеризатор удержал после кадра.
fn retained_after_big_blur(blur_scratch_retained_bytes: usize) -> usize {
    let mut limited = rasterizer(RasterCacheLimits {
        blur_scratch_retained_bytes,
        ..ROOMY_LIMITS
    });
    let overlay = render_ass(
        &mut limited,
        &single(r"{\an5\pos(640,360)\bord0\fs120\blur6}HHHH"),
    );
    // Размытие действительно произошло (иначе нечего удерживать).
    assert!(partial_pixels(&overlay, SubtitleBitmapKind::Glyph) > 0);
    limited.blur_scratch.retained_bytes()
}

#[test]
fn blur_scratch_retention_follows_the_configured_limit() {
    // Лимит 0 -> после кадра рабочая память освобождена; щедрый лимит -> плоскости удержаны
    // для следующего кадра. Лимит приходит из `RasterCacheLimits` (конфиг), не константа.
    assert_eq!(retained_after_big_blur(0), 0);
    assert!(retained_after_big_blur(64 * 1024 * 1024) > 0);
}
