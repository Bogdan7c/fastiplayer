//! Анимация `\t` через публичный `SubtitleRasterizer::render`: файл `.ass` на входе, момент
//! показа в запросе, картинки, счётчики работы и отчёт об изменениях на выходе.
//!
//! События длятся 10 секунд (`dialogue`), PlayRes 1280x720 совпадает с холстом.

use subtitle_core::SubtitleTime;

use super::ass::{fresh_source, glyph_ink};
use super::*;
use crate::ass::AssRenderOptions;
use crate::overlay::AnimationChange;
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

const WIDTH_PX: usize = 1280;

fn cues_of(text: &str) -> Vec<AssTestCue> {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[&dialogue(0, text)],
    );
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

fn at(millis: i64) -> AnimationChange {
    AnimationChange::At(SubtitleTime::from_millis(millis))
}

/// Ширина чернил букв.
fn ink_width(overlay: &SubtitleOverlay) -> i32 {
    let (left, _, right, _) = glyph_ink(overlay);
    right - left
}

fn glyph_rgb(overlay: &SubtitleOverlay) -> (u8, u8, u8) {
    let color = of_kind(overlay, SubtitleBitmapKind::Glyph)
        .first()
        .map(|bitmap| bitmap.color())
        .expect("glyph bitmap");
    (color.red, color.green, color.blue)
}

fn glyph_alpha(overlay: &SubtitleOverlay) -> u8 {
    of_kind(overlay, SubtitleBitmapKind::Glyph)
        .first()
        .map(|bitmap| bitmap.color().alpha)
        .expect("glyph bitmap")
}

fn rasterized(rasterizer: &SubtitleRasterizer) -> u64 {
    rasterizer.statistics().rasterized_cues
}

const GROWING_TEXT: &str = r"{\an7\pos(100,100)\bord0\fs50\t(0,1000,\fs150)}HHHH";

#[test]
fn animated_font_size_grows_between_the_moments_and_holds_after_t2() {
    let cues = cues_of(GROWING_TEXT);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let start = ink_width(&render_at(&mut rasterizer, &cues, 0));
    let middle = ink_width(&render_at(&mut rasterizer, &cues, 500));
    let end = ink_width(&render_at(&mut rasterizer, &cues, 1000));
    let later = ink_width(&render_at(&mut rasterizer, &cues, 6000));
    // Размер 50 -> 100 -> 150: ширина чернил пропорциональна (запас на подгонку краёв).
    assert!(start < middle && middle < end, "{start} {middle} {end}");
    assert!(
        (f64::from(middle) / f64::from(start) - 2.0).abs() < 0.06,
        "{start} {middle}"
    );
    assert!(
        (f64::from(end) / f64::from(start) - 3.0).abs() < 0.09,
        "{start} {end}"
    );
    assert_eq!(end, later);
}

#[test]
fn accel_two_makes_the_growth_start_slowly() {
    // Вес 0.25 на середине: размер 50 + 100 * 0.25 = 75 (при линейном было бы 100).
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\fs50\t(0,1000,2,\fs150)}HHHH");
    let linear = cues_of(GROWING_TEXT);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let start = ink_width(&render_at(&mut rasterizer, &cues, 0));
    let accelerated = ink_width(&render_at(&mut rasterizer, &cues, 500));
    let plain = ink_width(&render_at(&mut rasterizer, &linear, 500));
    assert!(
        (f64::from(accelerated) / f64::from(start) - 1.5).abs() < 0.05,
        "{start} {accelerated}"
    );
    assert!(accelerated < plain, "{accelerated} {plain}");
}

#[test]
fn animated_color_blends_each_channel_between_the_old_and_new_color() {
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\1c&H000000&\t(0,1000,\1c&HFFFFFF&)}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert_eq!(glyph_rgb(&render_at(&mut rasterizer, &cues, 0)), (0, 0, 0));
    // 0.5 * 255 = 127.5 -> усечение до 127 в каждом канале.
    assert_eq!(
        glyph_rgb(&render_at(&mut rasterizer, &cues, 500)),
        (127, 127, 127)
    );
    assert_eq!(
        glyph_rgb(&render_at(&mut rasterizer, &cues, 1000)),
        (255, 255, 255)
    );
}

#[test]
fn animated_alpha_changes_the_opacity_of_the_fill() {
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\1a&HFF&\t(0,1000,\1a&H00&)}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Прозрачность 255 -> 0: на старте картинок нет совсем, дальше непрозрачность растёт.
    assert!(render_at(&mut rasterizer, &cues, 0).is_empty());
    let quarter = glyph_alpha(&render_at(&mut rasterizer, &cues, 250));
    let three_quarters = glyph_alpha(&render_at(&mut rasterizer, &cues, 750));
    assert!(quarter < three_quarters, "{quarter} {three_quarters}");
    assert_eq!(glyph_alpha(&render_at(&mut rasterizer, &cues, 1000)), 255);
}

#[test]
fn tags_without_animation_inside_a_transform_apply_from_the_first_frame() {
    // `\b1` и `\pos` внутри `\t` применяются сразу даже при нулевом весе (до t1).
    let inside = cues_of(r"{\an7\bord0\t(500,1000,\b1\pos(300,200))}HHHH");
    let outside = cues_of(r"{\an7\bord0\b1\pos(300,200)}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let early = render_at(&mut rasterizer, &inside, 0);
    let expected = render_at(&mut rasterizer, &outside, 0);
    assert_eq!(glyph_ink(&early), glyph_ink(&expected));
    assert_eq!(
        early.composite_rgba_over_transparent(),
        expected.composite_rgba_over_transparent()
    );
}

#[test]
fn text_after_the_first_closing_paren_is_read_at_full_weight() {
    // Совместимость с VSFilter: `\fs150` стоит после первой `)`, это обычный тег блока.
    let quirk = cues_of(r"{\an7\pos(100,100)\bord0\fs50\t(0,1000,\clip(0,0,5000,5000)\fs150)}HHHH");
    let plain = cues_of(r"{\an7\pos(100,100)\bord0\fs150}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let at_start = render_at(&mut rasterizer, &quirk, 0);
    assert_eq!(
        ink_width(&at_start),
        ink_width(&render_at(&mut rasterizer, &plain, 0))
    );
}

#[test]
fn nested_transform_weights_replace_each_other() {
    // Внутренний `\t` (0..500) подменяет вес внешнего (0..1000): на 250 мс его вес 0.5,
    // у внешнего — 0.25. Размер идёт по внутреннему: 50 + 100 * 0.5 = 100.
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\fs50\t(0,1000,\t(0,500,\fs150)}HHHH");
    let reference = cues_of(r"{\an7\pos(100,100)\bord0\fs100}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let nested = ink_width(&render_at(&mut rasterizer, &cues, 250));
    let expected = ink_width(&render_at(&mut rasterizer, &reference, 0));
    assert_eq!(nested, expected);
}

#[test]
fn animation_change_reports_before_during_and_after_a_size_transform() {
    let cues = cues_of(r"{\an7\bord0\pos(100,100)\t(500,1500,\fs150)}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert_eq!(
        render_at(&mut rasterizer, &cues, 100).animation_change(),
        at(501)
    );
    assert_eq!(
        render_at(&mut rasterizer, &cues, 1000).animation_change(),
        AnimationChange::EveryFrame
    );
    assert_eq!(
        render_at(&mut rasterizer, &cues, 1500).animation_change(),
        AnimationChange::None
    );
}

#[test]
fn a_finished_transform_is_not_redrawn_and_is_not_redrawn_per_frame_before_it_starts() {
    let cues = cues_of(r"{\an7\bord0\pos(100,100)\t(500,1500,\fs150)}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_at(&mut rasterizer, &cues, 100);
    let after_first = rasterized(&rasterizer);
    // До `t1` вес 0: тот же ключ кэша, повторного рисования нет.
    render_at(&mut rasterizer, &cues, 300);
    assert_eq!(rasterized(&rasterizer), after_first);
    // После `t2` вес 1 навсегда: одна новая запись и больше ничего.
    render_at(&mut rasterizer, &cues, 2000);
    let finished = rasterized(&rasterizer);
    assert_eq!(finished, after_first + 1);
    let second = render_at(&mut rasterizer, &cues, 5000);
    assert_eq!(rasterized(&rasterizer), finished);
    assert_eq!(second.animation_change(), AnimationChange::None);
}

#[test]
fn transform_cue_never_collides_with_a_regular_cue() {
    // `\t` выключает поиск свободного места (`detect_collisions = 0`): реплика с `\t` не
    // сдвигается и не сдвигает соседей, а две обычные реплики раздвигаются.
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\bord0\t(0,1000,\fscx110)}HHHH"),
            &dialogue(0, r"{\bord0}HHHH"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_at(&mut rasterizer, &cues, 0);
    let regions = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    assert_eq!(regions.len(), 2);
    // Обе реплики стоят на одном и том же месте: перекрываются.
    assert_eq!(regions[0].origin().y, regions[1].origin().y);
}

// --- Прямоугольный клип под `\t`: стадия после кэша ---

const WIPE: &str =
    r"{\an7\pos(100,100)\bord0\fs120\clip(0,0,0,720)\t(0,1000,\clip(0,0,1280,720))}HHHH";

/// Медленная шторка: за первую секунду край клипа проходит только по тексту (x = 100..440).
const SLOW_WIPE: &str =
    r"{\an7\pos(100,100)\bord0\fs120\clip(0,0,0,720)\t(0,4000,\clip(0,0,1280,720))}HHHH";

fn pixels(overlay: &SubtitleOverlay) -> Vec<u8> {
    overlay.composite_rgba_over_transparent()
}

fn painted_columns(pixels: &[u8]) -> (usize, usize) {
    let mut first = usize::MAX;
    let mut last = 0;
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if pixel[3] != 0 {
            let column = index % WIDTH_PX;
            first = first.min(column);
            last = last.max(column);
        }
    }
    (first, last)
}

#[test]
fn clip_only_transform_wipes_the_text_in_without_redrawing_it() {
    let cues = cues_of(WIPE);
    let plain = cues_of(r"{\an7\pos(100,100)\bord0\fs120}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let full = pixels(&render_at(&mut rasterizer, &plain, 0));
    let (_, full_right) = painted_columns(&full);
    let after_plain = rasterized(&rasterizer);

    // Старт: клип нулевой ширины, ничего не видно.
    assert!(render_at(&mut rasterizer, &cues, 0).is_empty());
    let after_first = rasterized(&rasterizer);
    assert_eq!(after_first, after_plain + 1);

    let mut previous_right = 0;
    for moment in [150_i32, 300, 450, 600, 750, 900] {
        let overlay = render_at(&mut rasterizer, &cues, i64::from(moment));
        let frame = pixels(&overlay);
        let (_, right) = painted_columns(&frame);
        // Правый край видимой области растёт вместе с весом: x1 = 1280 * k.
        let clip_edge = (1280.0 * f64::from(moment) / 1000.0) as usize;
        assert!(right <= clip_edge, "{moment}: {right} > {clip_edge}");
        assert!(
            right >= previous_right,
            "{moment}: {right} < {previous_right}"
        );
        previous_right = right;
        // Внутри клипа пиксели те же, что без клипа (текст не перерисован иначе).
        for y in 0..720 {
            for x in 0..clip_edge.min(WIDTH_PX) {
                let offset = (y * WIDTH_PX + x) * 4;
                assert_eq!(
                    &frame[offset..offset + 4],
                    &full[offset..offset + 4],
                    "({x},{y}) на {moment}"
                );
            }
        }
        assert_eq!(overlay.animation_change(), AnimationChange::EveryFrame);
    }
    assert!(previous_right > 100, "{previous_right}");
    // Первый кадр показывает только начало первой буквы, не весь текст.
    let first_frame = pixels(&render_at(&mut rasterizer, &cues, 150));
    assert!(painted_columns(&first_frame).1 < full_right);
    // Клип двигался девять раз, а текст рисовался один: счётчик не вырос.
    assert_eq!(rasterized(&rasterizer), after_first);

    // Конец: клип раскрыт, видимо всё, дальше картинка стоит.
    let done = render_at(&mut rasterizer, &cues, 1000);
    assert_eq!(pixels(&done), full);
    assert_eq!(done.animation_change(), AnimationChange::None);
    assert_eq!(rasterized(&rasterizer), after_first);
}

#[test]
fn every_wipe_frame_is_a_distinct_picture() {
    let cues = cues_of(SLOW_WIPE);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let mut previous = pixels(&render_at(&mut rasterizer, &cues, 400));
    for moment in [500, 600, 700, 800] {
        let frame = pixels(&render_at(&mut rasterizer, &cues, moment));
        assert_ne!(frame, previous, "кадр {moment} совпал с предыдущим");
        previous = frame;
    }
}

#[test]
fn a_time_dependent_clip_does_not_freeze_between_frames() {
    // Тот же момент даёт ту же картинку, другой — другую, даже когда запрос повторяется
    // через быстрый путь: срок годности `EveryFrame` не позволяет выдать старый кадр.
    let cues = cues_of(SLOW_WIPE);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 400);
    let again = render_at(&mut rasterizer, &cues, 400);
    assert_eq!(pixels(&first), pixels(&again));
    let later = render_at(&mut rasterizer, &cues, 700);
    assert_ne!(pixels(&first), pixels(&later));
}

#[test]
fn inverse_clip_transform_cuts_a_growing_hole() {
    let cues = cues_of(
        r"{\an7\pos(100,100)\bord0\fs120\iclip(0,0,0,720)\t(0,1000,\iclip(0,0,1280,720))}HHHH",
    );
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let start = pixels(&render_at(&mut rasterizer, &cues, 0));
    let middle = pixels(&render_at(&mut rasterizer, &cues, 500));
    let end = render_at(&mut rasterizer, &cues, 1000);
    assert!(start.iter().skip(3).step_by(4).any(|alpha| *alpha != 0));
    // Дыра растёт: видимых пикселей остаётся всё меньше, в конце — ничего.
    let count = |frame: &[u8]| {
        frame
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] != 0)
            .count()
    };
    assert!(count(&middle) < count(&start));
    assert!(end.is_empty());
}

#[test]
fn mixed_transform_redraws_the_text_but_keeps_the_clip_per_frame() {
    // Размер идёт по весу `\t` (запись кэша на каждый вес), клип считается по часам кадра.
    let cues = cues_of(
        r"{\an7\pos(100,100)\bord0\fs60\clip(0,0,0,720)\t(0,1000,\fs120\clip(0,0,1280,720))}HHHH",
    );
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let before = rasterized(&rasterizer);
    let a = render_at(&mut rasterizer, &cues, 300);
    let b = render_at(&mut rasterizer, &cues, 600);
    assert_eq!(rasterized(&rasterizer), before + 2);
    let (_, right_a) = painted_columns(&pixels(&a));
    let (_, right_b) = painted_columns(&pixels(&b));
    assert!(right_a <= 384 && right_b <= 768, "{right_a} {right_b}");
    assert!(right_a < right_b);
}

#[test]
fn clip_only_transform_before_its_start_waits_for_t1() {
    let cues = cues_of(
        r"{\an7\pos(100,100)\bord0\fs80\clip(0,0,300,720)\t(2000,3000,\clip(0,0,1280,720))}HHHH",
    );
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_at(&mut rasterizer, &cues, 500);
    assert_eq!(overlay.animation_change(), at(2001));
    let (_, right) = painted_columns(&pixels(&overlay));
    assert!(right < 300, "{right}");
}
