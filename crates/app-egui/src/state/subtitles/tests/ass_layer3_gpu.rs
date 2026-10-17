//! ASS слой 3 до пикселей (сессия 10a): сценарий рядом с «видео» -> настоящий поток-рисовальщик
//! -> размещение -> настоящий GPU-проход -> readback. Проверяется то, что видит зритель:
//! клип обрезает текст в кадре, рисунок `\p` стоит там, где задан, `\blur` смягчает край,
//! `\iclip` оставляет дыру. Тесты пропускаются (с сообщением), если на машине нет GPU adapter-а.

use super::ass_animation_support::*;
use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the ASS layer 3 test");
                return;
            }
        }
    };
}

/// PlayRes сценариев равен окну: координаты тегов = пиксели кадра.
const PLAY_RES: (u32, u32) = (WINDOW_WIDTH, WINDOW_HEIGHT);
/// Момент внутри события [1 с; 3 с).
const SHOWN_AT_MILLIS: u64 = 1_500;

/// Прямоугольник пикселей кадра: `[left, right) x [top, bottom)`.
#[derive(Debug, Clone, Copy)]
struct FrameRect {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl FrameRect {
    fn contains(&self, x: u32, y: u32) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }
}

/// Кадр окна с одним событием `text`, показанным в [`SHOWN_AT_MILLIS`], и кадр того же окна
/// без субтитров (для сравнения). Каждому вызову нужен свой `media` (владелец на медиа).
fn frame_with_event(harness: &mut Harness, media_number: u64, text: &str) -> (Vec<u8>, Vec<u8>) {
    let media = instance(media_number);
    let script = animated_script(
        PLAY_RES,
        &[(
            "0:00:01.00".to_owned(),
            "0:00:03.00".to_owned(),
            text.to_owned(),
        )],
    );
    let (mut owner, _directory) = owner_with_script(media, &script);
    let rect = full_window_picture();
    let (_, video_only) = harness.frame(rect, None);
    let shown = render_placed(&mut owner, harness, media, rect, SHOWN_AT_MILLIS);
    (shown, video_only)
}

/// Сколько красных пикселей кадра лежит внутри (`inside == true`) или снаружи `area`.
fn red_pixels(pixels: &[u8], area: FrameRect, inside: bool) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(index, pixel)| {
            let (x, y) = (*index as u32 % WINDOW_WIDTH, *index as u32 / WINDOW_WIDTH);
            is_red(**pixel) && area.contains(x, y) == inside
        })
        .count()
}

/// Широкий красный текст по центру кадра; `tags` добавляются к тегам события.
fn wide_text(tags: &str) -> String {
    format!("{{\\an5\\pos(320,180)\\fs80\\c&H0000FF&{tags}}}WWWWWW")
}

/// Область клипа тестов: заметно уже текста, но пересекает его.
const CLIP_AREA: FrameRect = FrameRect {
    left: 240,
    top: 100,
    right: 400,
    bottom: 260,
};

#[test]
fn rectangular_clip_keeps_the_text_inside_the_rectangle_and_removes_it_outside() {
    let mut harness = gpu_or_skip!();
    let (unclipped, _) = frame_with_event(&mut harness, 11, &wide_text(""));
    let (clipped, _) = frame_with_event(&mut harness, 12, &wide_text("\\clip(240,100,400,260)"));

    let red_inside_before = red_pixels(&unclipped, CLIP_AREA, true);
    let red_outside_before = red_pixels(&unclipped, CLIP_AREA, false);
    assert!(
        red_inside_before > 200 && red_outside_before > 200,
        "без клипа текст шире области: внутри {red_inside_before}, снаружи {red_outside_before}"
    );
    // Снаружи прямоугольника красного нет вовсе, внутри остался тот же рисунок букв.
    assert_eq!(red_pixels(&clipped, CLIP_AREA, false), 0);
    assert_eq!(red_pixels(&clipped, CLIP_AREA, true), red_inside_before);
}

#[test]
fn inverse_clip_leaves_a_hole_where_the_text_would_be() {
    let mut harness = gpu_or_skip!();
    let (unclipped, _) = frame_with_event(&mut harness, 13, &wide_text(""));
    let (with_hole, _) = frame_with_event(&mut harness, 14, &wide_text("\\iclip(240,100,400,260)"));

    assert!(red_pixels(&unclipped, CLIP_AREA, true) > 200);
    // Дыра пуста, а вне дыры текст такой же, как без клипа.
    assert_eq!(red_pixels(&with_hole, CLIP_AREA, true), 0);
    assert_eq!(
        red_pixels(&with_hole, CLIP_AREA, false),
        red_pixels(&unclipped, CLIP_AREA, false)
    );
}

#[test]
fn drawing_reaches_the_frame_at_its_expected_screen_bounds() {
    let mut harness = gpu_or_skip!();
    // Квадрат 200x100 с углом в (100, 60): PlayRes равен окну, масштаб 1.
    let (shown, video_only) = frame_with_event(
        &mut harness,
        15,
        "{\\an7\\pos(100,60)\\c&H0000FF&\\p1}m 0 0 l 200 0 200 100 0 100",
    );

    assert_ne!(shown, video_only, "рисунок дошёл до кадра");
    let bounds = bounds_of(&shown, WINDOW_WIDTH, is_red).expect("red square in the frame");
    // Допуск в пиксель: край квадрата на границе пикселей.
    assert!(bounds.min_x.abs_diff(100) <= 1, "{bounds:?}");
    assert!(bounds.min_y.abs_diff(60) <= 1, "{bounds:?}");
    assert!(bounds.max_x.abs_diff(299) <= 1, "{bounds:?}");
    assert!(bounds.max_y.abs_diff(159) <= 1, "{bounds:?}");
    // Это сплошная заливка: красных пикселей почти вся площадь 200x100.
    assert!(count_of(&shown, is_red) >= 200 * 100 - 600);
}

/// Сколько пикселей строки `y` отличаются от фона видео, но не красные: полупрозрачный край.
fn soft_edge_pixels_in_row(shown: &[u8], video_only: &[u8], y: u32) -> usize {
    let row = (y * WINDOW_WIDTH * 4) as usize..((y + 1) * WINDOW_WIDTH * 4) as usize;
    shown[row.clone()]
        .as_chunks::<4>()
        .0
        .iter()
        .zip(video_only[row].as_chunks::<4>().0)
        .filter(|(pixel, background)| {
            let changed = pixel[..3]
                .iter()
                .zip(&background[..3])
                .any(|(shown, behind)| shown.abs_diff(*behind) > 8);
            changed && !is_red(**pixel)
        })
        .count()
}

#[test]
fn blur_makes_the_edge_of_a_figure_soft_in_the_frame() {
    let mut harness = gpu_or_skip!();
    let square = "m 0 0 l 120 0 120 120 0 120";
    let (sharp, video_only) = frame_with_event(
        &mut harness,
        16,
        &format!("{{\\an5\\pos(320,180)\\c&H0000FF&\\p1}}{square}"),
    );
    let (soft, _) = frame_with_event(
        &mut harness,
        17,
        &format!("{{\\an5\\pos(320,180)\\c&H0000FF&\\blur6\\p1}}{square}"),
    );

    // Строка через середину квадрата пересекает левый и правый края.
    let middle_row = 180;
    let sharp_band = soft_edge_pixels_in_row(&sharp, &video_only, middle_row);
    let soft_band = soft_edge_pixels_in_row(&soft, &video_only, middle_row);
    assert!(sharp_band <= 4, "резкий край: {sharp_band} промежуточных");
    assert!(
        soft_band >= 12 && soft_band > 3 * sharp_band,
        "размытый край шире: {soft_band} против {sharp_band}"
    );
    // Размытие не стёрло фигуру: сердцевина по-прежнему красная.
    assert!(count_of(&soft, is_red) > 120 * 120 / 2);
}
