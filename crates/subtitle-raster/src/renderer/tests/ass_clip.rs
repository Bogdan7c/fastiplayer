//! Клипы ASS (`\clip`, `\iclip`; прямоугольные и векторные) через публичный
//! `SubtitleRasterizer::render`: файл `.ass` на входе, пиксели холста на выходе.
//!
//! Главный приём: рисуем одну и ту же реплику без клипа и с клипом и сравниваем композицию
//! попиксельно (`composite_rgba_over_transparent`). Внутри видимой области клип обязан дать
//! ровно тот же пиксель, что без клипа, вне её — полностью прозрачный.

use std::sync::Arc;

use super::ass::{fresh_source, render_ass, single};
use super::*;
use crate::test_ass::{DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

const WIDTH_PX: usize = 1280;

/// Крупный текст без обводки: много плотных пикселей, одна картинка слоя.
fn text_cue(clip: &str) -> String {
    format!(r"{{\an7\pos(200,200)\bord0\fs150{clip}}}HHHH")
}

/// Композиция реплики на прозрачный холст.
fn rgba(text: &str) -> Vec<u8> {
    render_ass(&mut rasterizer(ROOMY_LIMITS), &single(text)).composite_rgba_over_transparent()
}

fn alpha_at(pixels: &[u8], x: usize, y: usize) -> u8 {
    pixels[(y * WIDTH_PX + x) * 4 + 3]
}

fn pixel_at(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
    let offset = (y * WIDTH_PX + x) * 4;
    [
        pixels[offset],
        pixels[offset + 1],
        pixels[offset + 2],
        pixels[offset + 3],
    ]
}

/// Сколько пикселей с ненулевой альфой.
fn painted(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] != 0)
        .count()
}

/// Все пиксели холста, где `inside` истинно, в `clipped` равны `plain`; где ложно — прозрачны.
fn assert_clipped_like(
    plain: &[u8],
    clipped: &[u8],
    inside: impl Fn(usize, usize) -> bool,
    what: &str,
) {
    for y in 0..720 {
        for x in 0..WIDTH_PX {
            let expected = if inside(x, y) {
                pixel_at(plain, x, y)
            } else {
                [0, 0, 0, 0]
            };
            let actual = pixel_at(clipped, x, y);
            // Полностью прозрачный пиксель может нести любой цвет: сравниваем только альфу.
            if expected[3] == 0 {
                assert_eq!(actual[3], 0, "{what}: пиксель ({x},{y}) должен быть пуст");
            } else {
                assert_eq!(actual, expected, "{what}: пиксель ({x},{y})");
            }
        }
    }
}

#[test]
fn rect_clip_keeps_exactly_the_pixels_inside_the_rectangle() {
    let plain = rgba(&text_cue(""));
    let clipped = rgba(&text_cue(r"\clip(260,230,520,330)"));
    assert!(painted(&plain) > 0);
    assert_clipped_like(
        &plain,
        &clipped,
        |x, y| (260..520).contains(&x) && (230..330).contains(&y),
        r"\clip",
    );
    // Контроль: внутри прямоугольника действительно есть что показывать.
    assert!(painted(&clipped) > 500, "{}", painted(&clipped));
}

#[test]
fn inverse_rect_clip_cuts_a_hole_and_keeps_everything_around_it() {
    let plain = rgba(&text_cue(""));
    let clipped = rgba(&text_cue(r"\iclip(260,230,520,330)"));
    assert_clipped_like(
        &plain,
        &clipped,
        |x, y| !((260..520).contains(&x) && (230..330).contains(&y)),
        r"\iclip",
    );
    assert!(painted(&clipped) < painted(&plain));
}

#[test]
fn rect_clip_scales_with_play_res() {
    // PlayRes 640x360 вдвое меньше холста 1280x720: углы клипа удваиваются.
    let file = ass_file(
        (640, 360),
        true,
        &[DEFAULT_STYLE_LINE],
        &[&dialogue(
            0,
            r"{\an7\pos(100,100)\bord0\fs75\clip(130,115,260,165)}HHHH",
        )],
    );
    let plain_file = ass_file(
        (640, 360),
        true,
        &[DEFAULT_STYLE_LINE],
        &[&dialogue(0, r"{\an7\pos(100,100)\bord0\fs75}HHHH")],
    );
    let draw = |text: &str| {
        render_ass(
            &mut rasterizer(ROOMY_LIMITS),
            &load_ass_cues(text, fresh_source()),
        )
        .composite_rgba_over_transparent()
    };
    let (plain, clipped) = (draw(&plain_file), draw(&file));
    assert_clipped_like(
        &plain,
        &clipped,
        |x, y| (260..520).contains(&x) && (230..330).contains(&y),
        "scaled clip",
    );
}

#[test]
fn rect_clip_partly_outside_the_frame_keeps_the_visible_part() {
    let plain = rgba(&text_cue(""));
    let clipped = rgba(&text_cue(r"\clip(-500,-500,400,300)"));
    assert_clipped_like(
        &plain,
        &clipped,
        |x, y| x < 400 && y < 300,
        "clip beyond the frame",
    );
    // Обратный: дыра вылезает за кадр, остальное цело.
    let inverse = rgba(&text_cue(r"\iclip(-500,-500,400,300)"));
    assert_clipped_like(
        &plain,
        &inverse,
        |x, y| !(x < 400 && y < 300),
        "iclip beyond the frame",
    );
}

#[test]
fn last_rect_clip_wins() {
    let last = rgba(&text_cue(r"\clip(0,0,100,100)\clip(260,230,520,330)"));
    let only = rgba(&text_cue(r"\clip(260,230,520,330)"));
    assert_eq!(last, only);
    // Режим тоже берётся от последнего тега.
    let mode = rgba(&text_cue(r"\clip(0,0,100,100)\iclip(260,230,520,330)"));
    assert_eq!(mode, rgba(&text_cue(r"\iclip(260,230,520,330)")));
}

/// Треугольник в координатах сценария, на пересечении с текстом `HHHH`.
const TRIANGLE: &str = "m 220 220 l 700 220 220 330";

/// Лежит ли точка строго внутри треугольника с запасом `margin` пикселей от краёв.
fn deep_inside_triangle(x: usize, y: usize, margin: f64) -> bool {
    let (px, py) = (x as f64, y as f64);
    // Рёбра: сверху y=220, слева x=220, гипотенуза (700,220)-(220,330).
    let slope = (330.0 - 220.0) / (220.0 - 700.0);
    let hypotenuse_y = 220.0 + slope * (px - 700.0);
    px >= 220.0 + margin && py >= 220.0 + margin && py <= hypotenuse_y - margin
}

fn far_outside_triangle(x: usize, y: usize, margin: f64) -> bool {
    let (px, py) = (x as f64, y as f64);
    let slope = (330.0 - 220.0) / (220.0 - 700.0);
    let hypotenuse_y = 220.0 + slope * (px - 700.0);
    px <= 220.0 - margin || py <= 220.0 - margin || py >= hypotenuse_y + margin
}

#[test]
fn vector_clip_keeps_the_text_inside_the_shape_and_removes_the_rest() {
    let plain = rgba(&text_cue(""));
    let clipped = rgba(&text_cue(&format!(r"\clip({TRIANGLE})")));
    let mut kept = 0;
    for y in 0..720 {
        for x in 0..WIDTH_PX {
            if deep_inside_triangle(x, y, 2.0) {
                assert_eq!(
                    pixel_at(&clipped, x, y),
                    pixel_at(&plain, x, y),
                    "({x},{y})"
                );
                kept += usize::from(alpha_at(&plain, x, y) != 0);
            } else if far_outside_triangle(x, y, 2.0) {
                assert_eq!(alpha_at(&clipped, x, y), 0, "({x},{y}) вне фигуры");
            }
        }
    }
    assert!(kept > 300, "в фигуре должен остаться текст: {kept}");
    assert!(painted(&clipped) < painted(&plain));
}

#[test]
fn inverse_vector_clip_removes_the_inside_of_the_shape() {
    let plain = rgba(&text_cue(""));
    let clipped = rgba(&text_cue(&format!(r"\iclip({TRIANGLE})")));
    let mut removed = 0;
    for y in 0..720 {
        for x in 0..WIDTH_PX {
            if deep_inside_triangle(x, y, 2.0) {
                assert_eq!(alpha_at(&clipped, x, y), 0, "({x},{y}) внутри фигуры");
                removed += usize::from(alpha_at(&plain, x, y) != 0);
            } else if far_outside_triangle(x, y, 2.0) {
                assert_eq!(
                    pixel_at(&clipped, x, y),
                    pixel_at(&plain, x, y),
                    "({x},{y})"
                );
            }
        }
    }
    assert!(removed > 300, "из фигуры должен уйти текст: {removed}");
}

#[test]
fn vector_clip_scale_divides_the_drawing_coordinates() {
    let scale_one = rgba(&text_cue(r"\clip(m 220 220 l 700 220 220 330)"));
    let scale_two = rgba(&text_cue(r"\clip(2,m 440 440 l 1400 440 440 660)"));
    // Квантование матрицы у масштабов 1 и 2 одинаково (множитель точно степень двойки).
    assert_eq!(scale_one, scale_two);
    assert!(painted(&scale_one) > 0);
}

#[test]
fn first_vector_clip_wins() {
    let first = rgba(&text_cue(&format!(
        r"\clip({TRIANGLE})\clip(m 0 0 l 50 0 50 50)"
    )));
    let only = rgba(&text_cue(&format!(r"\clip({TRIANGLE})")));
    assert_eq!(first, only);
}

#[test]
fn empty_vector_clip_hides_everything_and_empty_inverse_clip_hides_nothing() {
    // `m 0 0` — валидный рисунок без контуров: маска пуста.
    let plain = rgba(&text_cue(""));
    let inside = rgba(&text_cue(r"\clip(m 0 0)"));
    assert_eq!(painted(&inside), 0);
    let outside = rgba(&text_cue(r"\iclip(m 0 0)"));
    assert_eq!(outside, plain);
}

#[test]
fn rect_and_vector_clips_work_together() {
    let both = rgba(&text_cue(&format!(
        r"\clip(300,200,520,400)\clip({TRIANGLE})"
    )));
    let plain = rgba(&text_cue(""));
    for y in 0..720 {
        for x in 0..WIDTH_PX {
            let in_rect = (300..520).contains(&x) && (200..400).contains(&y);
            if !in_rect || far_outside_triangle(x, y, 2.0) {
                assert_eq!(alpha_at(&both, x, y), 0, "({x},{y}) вне пересечения");
            } else if deep_inside_triangle(x, y, 2.0) {
                assert_eq!(pixel_at(&both, x, y), pixel_at(&plain, x, y), "({x},{y})");
            }
        }
    }
    assert!(painted(&both) > 300);
}

#[test]
fn clip_applies_to_border_shadow_and_blur_layers() {
    let tags = r"\an7\pos(200,200)\bord6\shad8\blur3\fs150";
    let plain = rgba(&format!("{{{tags}}}HHHH"));
    let clipped = rgba(&format!(r"{{{tags}\clip(260,230,520,330)}}HHHH"));
    assert_clipped_like(
        &plain,
        &clipped,
        |x, y| (260..520).contains(&x) && (230..330).contains(&y),
        "clip with border, shadow, blur",
    );
}

#[test]
fn clip_on_a_rotated_text_and_a_drawing_cuts_what_is_drawn() {
    for body in [
        r"{\an7\pos(300,300)\bord0\fs150\frz20\clip(260,230,520,330)}HHHH",
        r"{\an7\pos(300,300)\bord0\p1\clip(260,230,520,330)}m 0 0 l 400 0 400 300 0 300{\p0}",
    ] {
        let unclipped = body.replace(r"\clip(260,230,520,330)", "");
        let (plain, clipped) = (rgba(&unclipped), rgba(body));
        assert!(painted(&plain) > 0);
        assert_clipped_like(
            &plain,
            &clipped,
            |x, y| (260..520).contains(&x) && (230..330).contains(&y),
            body,
        );
    }
}

/// Прямоугольная картинка-рисунок: вся её маска плотная, поэтому куски видны по размерам.
const SQUARE: &str = r"{\an7\pos(200,200)\bord0\p1}m 0 0 l 100 0 100 100 0 100{\p0}";

#[test]
fn inverse_rect_clip_splits_an_image_into_four_parts_in_libass_order() {
    let whole = render_ass(&mut rasterizer(ROOMY_LIMITS), &single(SQUARE));
    let [image] = whole.bitmaps() else {
        panic!("рисунок без обводки — одна картинка");
    };
    let (gx, gy) = (image.origin().x, image.origin().y);
    let (gw, gh) = (image.width() as i32, image.height() as i32);

    let clipped_text = SQUARE.replace(r"{\an7", r"{\an7\iclip(230,240,270,260)");
    let clipped = render_ass(&mut rasterizer(ROOMY_LIMITS), &single(&clipped_text));
    let parts: Vec<(i32, i32, i32, i32)> = clipped
        .bitmaps()
        .iter()
        .map(|part| {
            (
                part.origin().x,
                part.origin().y,
                part.width() as i32,
                part.height() as i32,
            )
        })
        .collect();
    // libass (`render_glyph_i`): слева, над дырой, под дырой, справа.
    assert_eq!(
        parts,
        vec![
            (gx, gy, 230 - gx, gh),
            (230, gy, 40, 240 - gy),
            (230, 260, 40, gy + gh - 260),
            (270, gy, gx + gw - 270, gh),
        ]
    );
}

#[test]
fn clipped_images_keep_their_owner_and_can_be_retained_by_cue() {
    let cues = single(&text_cue(r"\clip(260,230,520,330)"));
    let key = cues[0].key;
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &cues);
    assert!(!overlay.is_empty());
    assert!(
        overlay
            .bitmaps()
            .iter()
            .all(|image| image.cue_key() == Some(key))
    );
    assert!(overlay.retain_cues(|owner| owner != key).is_empty());
    assert_eq!(
        overlay.retain_cues(|owner| owner == key).bitmaps().len(),
        overlay.bitmaps().len()
    );
}

/// Две независимые реплики одного файла: клипнутая и обычная.
fn clipped_and_plain_pair() -> Vec<crate::test_ass::AssTestCue> {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(
                0,
                r"{\an7\pos(100,100)\bord2\fs100\clip(150,120,300,200)}Clipped",
            ),
            &dialogue(0, r"{\an7\pos(100,400)\bord2\fs100}Plain"),
        ],
    );
    load_ass_cues(&file, fresh_source())
}

fn bitmaps_of(overlay: &SubtitleOverlay, key: CueKey) -> Vec<&SubtitleBitmap> {
    overlay
        .bitmaps()
        .iter()
        .filter(|image| image.cue_key() == Some(key))
        .collect()
}

#[test]
fn clip_does_not_touch_the_pictures_of_other_cues() {
    let pair = clipped_and_plain_pair();
    let plain_key = pair[1].key;
    let together = render_ass(&mut rasterizer(ROOMY_LIMITS), &pair);
    let alone = render_ass(&mut rasterizer(ROOMY_LIMITS), &pair[1..]);
    let snapshot = |overlay: &SubtitleOverlay| -> Vec<(i32, i32, u32, u32, Vec<u8>)> {
        bitmaps_of(overlay, plain_key)
            .into_iter()
            .map(|image| {
                (
                    image.origin().x,
                    image.origin().y,
                    image.width(),
                    image.height(),
                    (0..image.height())
                        .filter_map(|row| image.coverage_row(row))
                        .flatten()
                        .copied()
                        .collect(),
                )
            })
            .collect()
    };
    assert!(!snapshot(&alone).is_empty());
    assert_eq!(snapshot(&together), snapshot(&alone));
}

#[test]
fn clip_is_applied_after_the_cache_and_does_not_rasterize_again() {
    let pair = clipped_and_plain_pair();
    let clipped_key = pair[0].key;
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_ass(&mut rasterizer, &pair[..1]);
    assert_eq!(rasterizer.statistics().rasterized_cues, 1);
    // Новая реплика в кадре: кадр собирается заново, но клипнутая реплика берётся из кэша.
    let second = render_ass(&mut rasterizer, &pair);
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.rasterized_cues, 2, "клипнутую рисуют один раз");
    assert_eq!(statistics.rendered_frames, 2);

    let before = bitmaps_of(&first, clipped_key);
    let after = bitmaps_of(&second, clipped_key);
    assert!(!before.is_empty());
    assert_eq!(before.len(), after.len());
    for (old, new) in before.iter().zip(&after) {
        // Маски общие: GPU-оболочка не перезагружает текстуры клипнутой реплики.
        assert!(Arc::ptr_eq(old.shared_coverage(), new.shared_coverage()));
    }
    // Повтор того же запроса — быстрый путь без работы.
    let third = render_ass(&mut rasterizer, &pair);
    assert_eq!(third.change(), OverlayChange::Unchanged);
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
}

#[test]
fn border_style_4_background_is_not_clipped() {
    let style = "Style: Box,Noto Sans,60,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,4,3,0,7,40,40,40,1";
    let make = |clip: &str| {
        let file = ass_file(
            (1280, 720),
            true,
            &[style],
            &[&format!(
                "Dialogue: 0,0:00:00.00,0:00:10.00,Box,,0,0,0,,{{\\an7\\pos(200,200){clip}}}Boxed text"
            )],
        );
        render_ass(
            &mut rasterizer(ROOMY_LIMITS),
            &load_ass_cues(&file, fresh_source()),
        )
    };
    let plain = make("");
    let clipped = make(r"\clip(210,210,260,260)");
    let background = |overlay: &SubtitleOverlay| {
        of_kind(overlay, SubtitleBitmapKind::Background)
            .into_iter()
            .map(|image| (image.origin(), image.width(), image.height()))
            .collect::<Vec<_>>()
    };
    assert_eq!(background(&plain).len(), 1, "у BorderStyle 4 есть подложка");
    // Как в libass (`add_background` после клипов): подложка остаётся целой.
    assert_eq!(background(&clipped), background(&plain));
    // А буквы обрезаны.
    assert!(
        of_kind(&clipped, SubtitleBitmapKind::Glyph)
            .iter()
            .all(|image| image.width() <= 50 && image.height() <= 50)
    );
}

#[test]
fn rejected_vector_clip_is_not_applied_and_is_counted() {
    // Точка рисунка вне допустимого диапазона: libass считает контур невалидным и не клипует.
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_ass(
        &mut rasterizer,
        &single(&text_cue(r"\clip(m 0 0 l 99999999999 0)")),
    );
    let plain = rgba(&text_cue(""));
    assert_eq!(overlay.composite_rgba_over_transparent(), plain);
    assert_eq!(rasterizer.statistics().invalid_ass_drawings, 1);
}

#[test]
fn vector_clip_wider_than_the_canvas_limit_still_clips_the_visible_part() {
    let mut limited = rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_canvas_side: 1_280,
            ..ROOMY_INPUT
        },
    });
    // Фигура 6000 x 2300 пикселей (шире предела стороны 1280) с нижним краем на y = 300:
    // текст (y 200..350) должен остаться только выше этого края. Раньше такой клип целиком
    // отбрасывался и текст оставался виден полностью.
    let plain = rgba(&text_cue(""));
    let overlay = render_ass(
        &mut limited,
        &single(&text_cue(
            r"\clip(m -2000 -2000 l 4000 -2000 4000 300 -2000 300)",
        )),
    );
    let clipped = overlay.composite_rgba_over_transparent();
    assert!(painted(&plain) > 0 && painted(&clipped) > 0);
    // С запасом на квантование матрицы клипа (около 0,06 % от половины размера фигуры).
    for y in 0..720 {
        for x in 0..WIDTH_PX {
            if y <= 292 {
                assert_eq!(
                    pixel_at(&clipped, x, y),
                    pixel_at(&plain, x, y),
                    "({x}, {y})"
                );
            } else if y >= 308 {
                assert_eq!(alpha_at(&clipped, x, y), 0, "({x}, {y})");
            }
        }
    }
    assert_eq!(limited.statistics().skipped_oversized_bitmaps, 0);
}

#[test]
fn inverse_vector_clip_wider_than_the_canvas_limit_cuts_the_hole() {
    let mut limited = rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_canvas_side: 1_280,
            ..ROOMY_INPUT
        },
    });
    let plain = rgba(&text_cue(""));
    let overlay = render_ass(
        &mut limited,
        &single(&text_cue(
            r"\iclip(m -2000 -2000 l 4000 -2000 4000 300 -2000 300)",
        )),
    );
    let clipped = overlay.composite_rgba_over_transparent();
    for y in 0..720 {
        for x in 0..WIDTH_PX {
            if y <= 292 {
                assert_eq!(alpha_at(&clipped, x, y), 0, "({x}, {y})");
            } else if y >= 308 {
                assert_eq!(
                    pixel_at(&clipped, x, y),
                    pixel_at(&plain, x, y),
                    "({x}, {y})"
                );
            }
        }
    }
    assert!(painted(&clipped) > 0);
}
