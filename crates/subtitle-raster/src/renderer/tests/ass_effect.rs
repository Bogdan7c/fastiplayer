//! Эффекты `Banner`, `Scroll up`, `Scroll down` через публичный `SubtitleRasterizer::render`:
//! файл `.ass` на входе, момент показа в запросе, положение и обрезка картинок на выходе.
//!
//! События длятся 10 секунд, холст 1280x720 и PlayRes 1280x720 (масштаб 1): сдвиг в пикселях
//! равен `мс / задержка`. Точное совпадение с libass закреплено эталонами `ass_fx_*`.

use subtitle_core::SubtitleTime;

use super::ass::{fresh_source, glyph_ink};
use super::*;
use crate::ass::AssRenderOptions;
use crate::overlay::AnimationChange;
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, load_ass_cues};

/// Событие стиля `Default` на 10 секунд с полем `Effect`.
fn effect_cues(effect: &str, text: &str) -> Vec<AssTestCue> {
    let line = format!("Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,{effect},{text}");
    let file = ass_file((1280, 720), true, &[DEFAULT_STYLE_LINE], &[&line]);
    load_ass_cues(&file, fresh_source())
}

fn render_at(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    moment_ms: i64,
) -> SubtitleOverlay {
    let list: Vec<RenderableCue<'_>> = cues.iter().map(AssTestCue::renderable).collect();
    let appearance = SubtitleAppearance::youtube_like();
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::from_millis(moment_ms),
            cues: &list,
            canvas: canvas(),
            appearance: &appearance,
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

/// Рамка видимых букв реплики на момент `moment_ms`.
fn ink_at(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    moment_ms: i64,
) -> (i32, i32, i32, i32) {
    glyph_ink(&render_at(rasterizer, cues, moment_ms))
}

const BANNER_TEXT: &str = r"{\bord0\fs60}HHHH";

#[test]
fn banner_right_to_left_moves_left_by_the_expected_step_and_keeps_its_row() {
    // 10 мс на пиксель: за секунду 100 px.
    let cues = effect_cues("Banner;10;0;0", BANNER_TEXT);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = ink_at(&mut rasterizer, &cues, 1000);
    let second = ink_at(&mut rasterizer, &cues, 2000);
    let third = ink_at(&mut rasterizer, &cues, 3500);
    // Блок начинается в `PlayResX - shift`: чуть правее из-за левого зазора буквы.
    assert!(
        (1280 - 100..1280 - 100 + 12).contains(&first.0),
        "first {first:?}"
    );
    assert_eq!(first.0 - second.0, 100, "{first:?} {second:?}");
    assert_eq!(second.0 - third.0, 150, "{second:?} {third:?}");
    // Строка остаётся на месте: прокрутка не двигает текст по вертикали.
    assert_eq!((first.1, first.3), (second.1, second.3));
    assert_eq!((first.1, first.3), (third.1, third.3));
}

#[test]
fn banner_left_to_right_moves_right_by_the_expected_step() {
    let cues = effect_cues("Banner;10;1;0", BANNER_TEXT);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = ink_at(&mut rasterizer, &cues, 2000);
    let second = ink_at(&mut rasterizer, &cues, 3000);
    // Правый край блока стоит в `shift`: справа от букв остаётся лишь зазор в пару пикселей.
    assert!(
        (200 - 12..=200).contains(&first.2),
        "right edge {}",
        first.2
    );
    assert_eq!(second.2 - first.2, 100, "{first:?} {second:?}");
    assert_eq!(second.0 - first.0, 100);
}

#[test]
fn banner_ignores_wrap_and_keeps_a_long_line_on_one_row() {
    let long = r"{\bord0\fs60}This banner line is far too long to fit on one row of the frame and must not wrap";
    let cues = effect_cues("Banner;10;0", long);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_at(&mut rasterizer, &cues, 500);
    let (_, top, _, bottom) = glyph_ink(&overlay);
    // Одна строка: высота рамки не больше размера шрифта с запасом на выносные элементы.
    assert!(bottom - top < 90, "ink height {}", bottom - top);
}

#[test]
fn banner_picture_changes_every_frame_and_asks_to_be_redrawn() {
    let cues = effect_cues("Banner;10;0", BANNER_TEXT);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let mut previous_left = None;
    for moment_ms in [1000, 1040, 1080, 1120] {
        let overlay = render_at(&mut rasterizer, &cues, moment_ms);
        assert_eq!(
            overlay.animation_change(),
            AnimationChange::EveryFrame,
            "at {moment_ms} ms"
        );
        let left = glyph_ink(&overlay).0;
        if let Some(previous) = previous_left {
            // 40 мс при 10 мс на пиксель = 4 px влево на каждом кадре.
            assert_eq!(previous - left, 4, "at {moment_ms} ms");
        }
        previous_left = Some(left);
    }
}

/// Двухстрочный блок: высота строки 80 px, буквы «H» занимают в ней 39 px начиная с 21-го.
const TWO_LINES: &str = r"{\bord0\fs80}HHHH\NHHHH";

#[test]
fn scroll_up_clips_the_text_to_the_band_at_both_ends() {
    let cues = effect_cues("Scroll up;200;400;20", TWO_LINES);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // shift = ms / 20; блок поднимается от нижней границы полосы 400 (верх блока = 400 - shift).
    let entering = ink_at(&mut rasterizer, &cues, 2400);
    assert!(
        entering.1 >= 200 && entering.3 == 400,
        "the second line is cut by the bottom band edge: {entering:?}"
    );
    let inside = ink_at(&mut rasterizer, &cues, 4000);
    assert!(
        inside.1 > 200 && inside.3 < 400,
        "fully inside the band: {inside:?}"
    );
    let leaving = ink_at(&mut rasterizer, &cues, 4800);
    assert!(
        leaving.1 == 200 && leaving.3 < 400,
        "the first line is cut by the top band edge: {leaving:?}"
    );
    // Движение вверх: нижний край букв поднимается от момента к моменту.
    assert!(entering.3 > inside.3 && inside.3 > leaving.3);
}

#[test]
fn scroll_down_moves_down_inside_the_band() {
    let cues = effect_cues("Scroll down;200;400;20", TWO_LINES);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Нижний край блока стоит в `200 + shift`; сначала блок выезжает из верхней границы полосы.
    let entering = ink_at(&mut rasterizer, &cues, 2400);
    assert!(
        entering.1 == 200 && entering.3 < 400,
        "the first line is cut by the top band edge: {entering:?}"
    );
    let leaving = ink_at(&mut rasterizer, &cues, 4800);
    assert!(
        leaving.1 > 200 && leaving.3 == 400,
        "the second line is cut by the bottom band edge: {leaving:?}"
    );
}

#[test]
fn effect_with_too_few_numbers_is_ignored_and_the_picture_is_static() {
    let plain = effect_cues("", BANNER_TEXT);
    let broken = effect_cues("Scroll up;100;300", BANNER_TEXT);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let expected = glyph_ink(&render_at(&mut rasterizer, &plain, 1000));
    let early = render_at(&mut rasterizer, &broken, 1000);
    let late = render_at(&mut rasterizer, &broken, 4000);
    assert_eq!(glyph_ink(&early), expected);
    assert_eq!(glyph_ink(&late), expected);
    assert_eq!(late.animation_change(), AnimationChange::None);
}

/// Экранные строки, в которых у букв (без обводки и тени) есть закрашенные пиксели.
fn glyph_ink_rows(overlay: &SubtitleOverlay) -> std::collections::BTreeSet<i32> {
    let mut rows = std::collections::BTreeSet::new();
    for bitmap in overlay
        .bitmaps()
        .iter()
        .filter(|bitmap| bitmap.kind() == SubtitleBitmapKind::Glyph)
    {
        for row in 0..bitmap.height() {
            let inked = bitmap
                .coverage_row(row)
                .is_some_and(|pixels| pixels.iter().any(|coverage| *coverage > 0));
            if inked {
                rows.insert(bitmap.origin().y + i32::try_from(row).unwrap_or(i32::MAX));
            }
        }
    }
    rows
}

/// `\iclip` внутри полосы `Scroll up`: дыра вырезает свои строки, остальные строки полосы
/// остаются (libass: клип по высоте пересекается с прямоугольником, дыра — это пересечение).
/// Эталон `ass_fx_scroll_iclip`.
#[test]
fn scroll_up_with_inverse_clip_cuts_the_hole_rows_out_of_the_band() {
    let cues = effect_cues(
        "Scroll up;200;400;20",
        r"{\iclip(0,240,1280,320)}{\bord0\fs80}HHHH\NHHHH",
    );
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Момент 4000 мс: сдвиг 200 px, блок целиком в полосе: буквы строки 1 на 221..260,
    // строки 2 на 301..340 (см. TWO_LINES).
    let rows = glyph_ink_rows(&render_at(&mut rasterizer, &cues, 4000));
    assert!(
        rows.iter().all(|row| !(240..320).contains(row)),
        "the hole rows 240..320 must stay empty: {rows:?}"
    );
    assert!(
        rows.contains(&221) && rows.contains(&239),
        "the first line above the hole survives: {rows:?}"
    );
    assert!(
        rows.contains(&320) && rows.contains(&339),
        "the second line below the hole survives: {rows:?}"
    );
}

/// Дыра `\iclip` шире полосы: libass пересекает с полосой саму дыру, поэтому часть блока вне
/// полосы НЕ обрезается (она вне дыры) и видна, а внутри полосы всё вырезано.
#[test]
fn scroll_up_inverse_clip_wider_than_the_band_shows_the_text_outside_the_band() {
    let cues = effect_cues(
        "Scroll up;200;400;20",
        r"{\iclip(0,0,1280,720)}{\bord0\fs80}HHHH\NHHHH",
    );
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Момент 2400 мс: блок на 280..440; вторая строка букв на 381..420 пересекает низ полосы 400.
    let rows = glyph_ink_rows(&render_at(&mut rasterizer, &cues, 2400));
    assert_eq!(
        rows.iter().next().copied(),
        Some(400),
        "everything above the band bottom is inside the shrunk hole: {rows:?}"
    );
    assert_eq!(rows.iter().next_back().copied(), Some(419), "{rows:?}");
}
