//! Векторные рисунки ASS (`\p`, `\pbo`) через публичный `SubtitleRasterizer::render`: файл `.ass`
//! на входе, картинки на холсте на выходе. Проверяется то, что видно на экране: где стоят
//! закрашенные пиксели, насколько они закрашены и как рисунок влияет на соседний текст.

use super::ass::{render_ass, render_ass_with, single};
use super::*;
use crate::ass::{AssRenderOptions, AssStyleSource};

/// Квадрат 200x100 пикселей сценария с углом в начале координат.
const RECTANGLE: &str = "m 0 0 l 200 0 200 100 0 100";

/// Покрытие, начиная с которого пиксель считается «закрашенным»: растеризатор libass
/// квантует матрицу (масштаб с точностью около 0,06 %), поэтому у границы фигуры стоят
/// слабые пиксели покрытия 1-3 из 255 — их границей фигуры не считаем.
const SOLID_COVERAGE: u8 = 128;

/// Рамка пикселей картинки с покрытием не меньше [`SOLID_COVERAGE`].
fn solid_bounds_of(bitmap: &SubtitleBitmap) -> Option<(i32, i32, i32, i32)> {
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for row in 0..bitmap.height() {
        for (column, value) in bitmap.coverage_row(row)?.iter().enumerate() {
            if *value < SOLID_COVERAGE {
                continue;
            }
            let x = bitmap.origin().x + column as i32;
            let y = bitmap.origin().y + row as i32;
            bounds = Some(match bounds {
                None => (x, y, x + 1, y + 1),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
            });
        }
    }
    bounds
}

/// Рамка плотных пикселей каждой картинки вида `kind` по порядку отрисовки.
fn solid_bounds_per_bitmap(
    overlay: &SubtitleOverlay,
    kind: SubtitleBitmapKind,
) -> Vec<(i32, i32, i32, i32)> {
    of_kind(overlay, kind)
        .into_iter()
        .filter_map(solid_bounds_of)
        .collect()
}

/// Рамка плотных пикселей всех картинок вида `kind`.
fn solid_bounds(overlay: &SubtitleOverlay, kind: SubtitleBitmapKind) -> (i32, i32, i32, i32) {
    solid_bounds_per_bitmap(overlay, kind)
        .into_iter()
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        .expect("solid pixels")
}

fn glyph_ink(overlay: &SubtitleOverlay) -> (i32, i32, i32, i32) {
    solid_bounds(overlay, SubtitleBitmapKind::Glyph)
}

fn glyph_inks(overlay: &SubtitleOverlay) -> Vec<(i32, i32, i32, i32)> {
    solid_bounds_per_bitmap(overlay, SubtitleBitmapKind::Glyph)
}

/// Покрытие пикселя `(x, y)` картинками вида `kind` (0, если пиксель никто не закрашивает).
fn coverage_at(overlay: &SubtitleOverlay, kind: SubtitleBitmapKind, x: i32, y: i32) -> u8 {
    of_kind(overlay, kind)
        .into_iter()
        .filter_map(|bitmap| {
            let column = usize::try_from(x - bitmap.origin().x).ok()?;
            let row = u32::try_from(y - bitmap.origin().y).ok()?;
            bitmap.coverage_row(row)?.get(column).copied()
        })
        .max()
        .unwrap_or(0)
}

/// Реплика в левом верхнем углу в точке `(100, 100)`, без обводки.
fn at_top_left(text: &str) -> Vec<crate::test_ass::AssTestCue> {
    single(&format!(r"{{\an7\pos(100,100)\bord0}}{text}"))
}

fn render(text: &str) -> SubtitleOverlay {
    render_ass(&mut rasterizer(ROOMY_LIMITS), &at_top_left(text))
}

#[test]
fn p1_rectangle_is_a_filled_shape_at_the_anchor_without_any_letters() {
    let overlay = render(&format!(r"{{\p1}}{RECTANGLE}"));
    // Левый верхний угол рисунка стоит ровно в точке \pos (никакого сдвига по x_min).
    assert_eq!(glyph_ink(&overlay), (100, 100, 300, 200));
    // Внутри — (почти) полное покрытие, снаружи — не больше слабых краёв квантования.
    for (x, y) in [(101, 101), (298, 198), (200, 150)] {
        assert!(
            coverage_at(&overlay, SubtitleBitmapKind::Glyph, x, y) >= 250,
            "{x},{y}"
        );
    }
    for (x, y) in [(98, 150), (301, 150), (200, 98), (200, 201)] {
        assert_eq!(
            coverage_at(&overlay, SubtitleBitmapKind::Glyph, x, y),
            0,
            "{x},{y}"
        );
    }
}

#[test]
fn p_scale_divides_the_coordinates_by_a_power_of_two() {
    let full = glyph_ink(&render(&format!(r"{{\p1}}{RECTANGLE}")));
    let halved = glyph_ink(&render(r"{\p2}m 0 0 l 400 0 400 200 0 200"));
    let eighth = glyph_ink(&render(r"{\p4}m 0 0 l 1600 0 1600 800 0 800"));
    assert_eq!(halved, full);
    assert_eq!(eighth, full);
}

#[test]
fn bezier_and_spline_curves_are_filled() {
    // Круг из четырёх кубических кривых Безье диаметром 100.
    let circle = render(
        r"{\p1}m 50 0 b 78 0 100 22 100 50 b 100 78 78 100 50 100 b 22 100 0 78 0 50 b 0 22 22 0 50 0",
    );
    let ink = glyph_ink(&circle);
    assert!(
        (ink.0 - 100).abs() <= 1 && (ink.1 - 100).abs() <= 1,
        "{ink:?}"
    );
    assert!(
        (ink.2 - 200).abs() <= 1 && (ink.3 - 200).abs() <= 1,
        "{ink:?}"
    );
    assert_eq!(
        coverage_at(&circle, SubtitleBitmapKind::Glyph, 150, 150),
        255
    );
    assert_eq!(
        coverage_at(&circle, SubtitleBitmapKind::Glyph, 102, 102),
        0,
        "угол пуст"
    );

    // Замкнутый b-сплайн по углам квадрата: закрашена середина, угол квадрата срезан.
    let spline = render(r"{\p1}m 0 0 s 100 0 100 100 0 100 c");
    assert_eq!(
        coverage_at(&spline, SubtitleBitmapKind::Glyph, 150, 150),
        255
    );
    assert_eq!(coverage_at(&spline, SubtitleBitmapKind::Glyph, 100, 100), 0);
}

#[test]
fn two_contours_and_n_command_follow_the_nonzero_fill() {
    let two = render(r"{\p1}m 0 0 l 50 0 50 50 0 50 m 100 0 l 150 0 150 50 100 50");
    let inks = glyph_ink(&two);
    assert_eq!(inks, (100, 100, 250, 150));
    assert_eq!(
        coverage_at(&two, SubtitleBitmapKind::Glyph, 125, 125),
        255,
        "первый квадрат"
    );
    assert_eq!(
        coverage_at(&two, SubtitleBitmapKind::Glyph, 175, 125),
        0,
        "между контурами пусто"
    );
    assert_eq!(
        coverage_at(&two, SubtitleBitmapKind::Glyph, 225, 125),
        255,
        "второй квадрат"
    );
}

#[test]
fn baseline_offset_moves_neighbouring_text_but_not_the_drawing_itself() {
    // Буква H стоит на базовой линии (спуска нет), рисунок без \pbo тоже ставится на неё.
    let flat = render(r"H{\p1}m 0 0 l 20 0 20 20 0 20");
    let lowered = render(r"H{\p1\pbo15}m 0 0 l 20 0 20 20 0 20");
    let (flat_inks, lowered_inks) = (glyph_inks(&flat), glyph_inks(&lowered));
    assert_eq!((flat_inks.len(), lowered_inks.len()), (2, 2));
    // Базовая линия дробная, поэтому края сравниваются с точностью до пикселя.
    let near = |actual: i32, expected: i32| (actual - expected).abs() <= 1;
    // Без \pbo низ рисунка лежит на базовой линии буквы.
    assert!(near(flat_inks[1].3, flat_inks[0].3), "{flat_inks:?}");
    // С \pbo15 низ рисунка на 15 пикселей ниже базовой линии: буква осталась на ней.
    assert!(
        near(lowered_inks[1].3 - lowered_inks[0].3, 15),
        "{lowered_inks:?}"
    );
    // Размер самого рисунка не изменился.
    assert!(
        near(lowered_inks[1].3 - lowered_inks[1].1, 20),
        "{lowered_inks:?}"
    );
    assert!(near(flat_inks[1].3 - flat_inks[1].1, 20), "{flat_inks:?}");
}

#[test]
fn negative_baseline_offset_lifts_the_drawing_above_the_baseline() {
    let lifted = render(r"H{\p1\pbo-12}m 0 0 l 20 0 20 20 0 20");
    let inks = glyph_inks(&lifted);
    assert_eq!(inks.len(), 2);
    assert!(
        (inks[0].3 - inks[1].3 - 12).abs() <= 1,
        "низ рисунка на 12 px выше базовой линии: {inks:?}"
    );
}

#[test]
fn drawing_between_letters_advances_the_pen_by_its_width() {
    let plain = render("abcd");
    let with_drawing = render(r"ab{\p1}m 0 0 l 20 0 20 20 0 20{\p0}cd");
    let plain_ink = ink_bounds(plain.bitmaps()).expect("plain ink");
    let drawing_ink = ink_bounds(with_drawing.bitmaps()).expect("ink");
    // Все четыре буквы нарисованы, а хвост сдвинут ровно на ширину рисунка (20 px).
    assert_eq!(glyph_inks(&with_drawing).len(), 3, "ab, рисунок, cd");
    assert_eq!(drawing_ink.2 - plain_ink.2, 20);
    assert_eq!(drawing_ink.0, plain_ink.0, "левый край ab не двигается");
}

#[test]
fn drawing_height_takes_part_in_line_metrics_and_wrapping_to_the_next_line() {
    let overlay = render(r"{\p1}m 0 0 l 50 0 50 100 0 100{\p0}\NH");
    let inks = glyph_inks(&overlay);
    assert_eq!(inks.len(), 2);
    assert_eq!(inks[0], (100, 100, 150, 200));
    assert!(
        inks[1].1 >= 200,
        "вторая строка начинается под рисунком: {inks:?}"
    );
}

#[test]
fn border_and_shadow_follow_the_drawing_outline() {
    let cues = single(r"{\an7\pos(100,100)\bord4\shad6\p1}m 0 0 l 200 0 200 100 0 100");
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &cues);
    assert_eq!(glyph_ink(&overlay), (100, 100, 300, 200));
    let outline = solid_bounds(&overlay, SubtitleBitmapKind::Outline);
    assert_eq!(outline, (96, 96, 304, 204));
    let shadow = solid_bounds(&overlay, SubtitleBitmapKind::Shadow);
    assert_eq!(
        shadow,
        (102, 102, 310, 210),
        "тень — обводка, сдвинутая на 6 px"
    );
}

#[test]
fn rotation_and_stretch_apply_to_the_drawing_like_to_text() {
    let cues = |tags: &str| single(&format!(r"{{\an5\pos(640,360)\bord0{tags}\p1}}{RECTANGLE}"));
    let ink = |tags: &str| glyph_ink(&render_ass(&mut rasterizer(ROOMY_LIMITS), &cues(tags)));
    let size = |ink: (i32, i32, i32, i32)| (ink.2 - ink.0, ink.3 - ink.1);

    assert_eq!(size(ink("")), (200, 100));
    assert_eq!(size(ink(r"\fscx200")), (400, 100));
    assert_eq!(size(ink(r"\fscy50")), (200, 50));
    let turned = ink(r"\frz90");
    assert!(
        (size(turned).0 - 100).abs() <= 1 && (size(turned).1 - 200).abs() <= 1,
        "{turned:?}"
    );
    // Центр рисунка остаётся в опорной точке \an5\pos.
    assert!((turned.0 + turned.2 - 1280).abs() <= 2 && (turned.1 + turned.3 - 720).abs() <= 2);
}

#[test]
fn fade_applies_to_the_drawing_colour() {
    let opaque = render(&format!(r"{{\p1}}{RECTANGLE}"));
    let fading = render(&format!(r"{{\alpha&H80&\p1}}{RECTANGLE}"));
    let colour_alpha =
        |overlay: &SubtitleOverlay| of_kind(overlay, SubtitleBitmapKind::Glyph)[0].color().alpha;
    assert_eq!(colour_alpha(&opaque), 255);
    assert_eq!(colour_alpha(&fading), 127);
}

#[test]
fn invalid_drawings_are_counted_and_do_not_break_the_other_text() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let plain = render_ass(&mut rasterizer, &at_top_left("Hello"));
    let plain_ink = glyph_ink(&plain);
    let cases = [
        // `n` до `m`: libass отвергает рисунок целиком.
        r"{\p1}n 1 1 l 5 5",
        // Одни перемещения: рисовать нечего.
        r"{\p1}m 5 5",
        // Мусор вместо команд.
        r"{\p1}garbage without numbers",
        // Координата за пределом допустимого диапазона контура.
        r"{\p1}m 0 0 l 99999999999 0",
    ];
    for (index, drawing) in cases.iter().enumerate() {
        let overlay = render_ass(
            &mut rasterizer,
            &at_top_left(&format!("Hello{drawing}{{\\p0}}")),
        );
        assert_eq!(glyph_ink(&overlay), plain_ink, "{drawing}");
        assert_eq!(
            rasterizer.statistics().invalid_ass_drawings,
            index as u64 + 1,
            "{drawing}"
        );
    }
}

#[test]
fn drawing_text_is_cached_with_the_cue_and_counted_once() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let cues = at_top_left(r"{\p1}n 1 1");
    for _ in 0..3 {
        render_ass(&mut rasterizer, &cues);
    }
    assert_eq!(rasterizer.statistics().invalid_ass_drawings, 1);
}

#[test]
fn escapes_inside_drawing_text_are_ignored_commands_not_line_breaks() {
    // `\N` и `\h` внутри рисунка — сырой текст: перевода строки нет, а символы `\`, `N`, `h`
    // пропускаются как неизвестные команды (число после `\N` без своей команды потерялось
    // бы, поэтому перед каждым участком снова стоит `l`).
    let overlay = render(r"{\p1}m 0 0 l 200 0\N l 200 100\h l 0 100");
    assert_eq!(glyph_ink(&overlay), (100, 100, 300, 200));
}

#[test]
fn user_style_skips_author_drawings_but_keeps_letters() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let options = AssRenderOptions {
        style_source: AssStyleSource::User,
        ..AssRenderOptions::default()
    };
    let cues = at_top_left(&format!(r"{{\p1}}{RECTANGLE}{{\p0}}Hi"));
    let overlay = render_ass_with(
        &mut rasterizer,
        &cues,
        options,
        &SubtitleAppearance::outline_and_shadow(),
        canvas(),
    );
    let inks = glyph_inks(&overlay);
    assert_eq!(inks.len(), 1, "остались только буквы");
    assert!(inks[0].2 - inks[0].0 < 100, "это не прямоугольник 200x100");
    assert_eq!(rasterizer.statistics().invalid_ass_drawings, 0);
}

#[test]
fn drawing_far_larger_than_any_canvas_is_handled_like_libass_without_failing_the_cue() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Фигура в 4 миллиона пикселей выходит за диапазон квантования преобразования: libass
    // молча не строит её битмап, и реплика не считается сбойной. Строка при этом получает
    // гигантскую высоту, поэтому соседний текст уезжает за кадр.
    let overlay = render_ass(
        &mut rasterizer,
        &at_top_left(r"{\p1}m 0 0 l 4000000 0 4000000 4000000 0 4000000{\p0}ok"),
    );
    let statistics = rasterizer.statistics();
    assert_eq!((statistics.rasterized_cues, statistics.failed_cues), (1, 0));
    assert_eq!(statistics.invalid_ass_drawings, 0);
    assert!(overlay.is_empty(), "ничего не попало в кадр");
}
