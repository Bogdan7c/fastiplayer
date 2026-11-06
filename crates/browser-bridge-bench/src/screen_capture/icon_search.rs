//! Поиск значка экстеншена на кадре экрана (чистый код: ни процессов, ни файлов).
//!
//! Браузер рисует значок на панели размером 16–32 px и сам его масштабирует, поэтому
//! побайтовое сравнение не годится. Берём шаблоны значка разных размеров и сравниваем только
//! НЕПРОЗРАЧНЫЕ пиксели шаблона (края с полупрозрачностью смешиваются с фоном панели, а он
//! зависит от темы): средняя разница цветовых каналов должна быть малой. Поиск ограничен
//! полосой панели у верхнего края окна.

use super::ScreenError;
use super::frame::{Frame, RgbaImage};
use crate::desktop_input::Point;

/// Пиксель шаблона считается непрозрачным (участвует в сравнении) при alpha не ниже этого.
const OPAQUE_ALPHA: u8 = 250;
/// Шаблон с меньшим числом непрозрачных пикселей слишком бедный: совпал бы с чем угодно.
const MIN_OPAQUE_PIXELS: usize = 40;
/// Допустимая средняя разница по каналу (из 255), при которой считаем «это он».
/// Подобрано по настоящим снимкам Chromium и Firefox; у чужого значка разница в разы больше.
pub const DEFAULT_MAX_MEAN_DIFFERENCE: f32 = 28.0;

/// Непрозрачный пиксель шаблона.
#[derive(Debug, Clone, Copy)]
struct OpaquePixel {
    dx: u32,
    dy: u32,
    color: [u8; 3],
}

/// Значок одного размера, готовый к сравнению.
#[derive(Debug, Clone)]
pub struct IconTemplate {
    size: u32,
    opaque_pixels: Vec<OpaquePixel>,
}

impl IconTemplate {
    /// Шаблон из квадратного изображения с прозрачностью; `None`, если непрозрачных
    /// пикселей слишком мало для надёжного сравнения.
    pub fn from_image(image: &RgbaImage) -> Option<Self> {
        let mut opaque_pixels = Vec::new();
        for dy in 0..image.height {
            for dx in 0..image.width {
                let start = (dy as usize * image.width as usize + dx as usize) * 4;
                if image.rgba[start + 3] >= OPAQUE_ALPHA {
                    opaque_pixels.push(OpaquePixel {
                        dx,
                        dy,
                        color: [
                            image.rgba[start],
                            image.rgba[start + 1],
                            image.rgba[start + 2],
                        ],
                    });
                }
            }
        }
        let opaque_pixels = scouts_first(opaque_pixels);
        (opaque_pixels.len() >= MIN_OPAQUE_PIXELS).then_some(Self {
            size: image.width.max(image.height),
            opaque_pixels,
        })
    }
}

/// Сколько первых пикселей идёт «разведкой»: по ним положение отвергается, не считая остальные.
const PRESCREEN_PIXELS: usize = 16;
/// Разведка отвергает положение, если средняя разница на ней выше порога во столько раз
/// (запас на масштаб и сглаживание краёв: у настоящих находок на разведке порядка 10–20).
const PRESCREEN_REJECT_FACTOR: f32 = 2.0;

/// Порядок сравнения: сначала «разведчики» (самые отличные от среднего цвета пиксели и равномерная
/// выборка из остальных), затем все прочие. Белая страница проходит яркие пиксели, но не тёмные, а
/// чёрное поле экрана не проходит яркие: положение отвергается за десятки пикселей, а не за
/// сотни (иначе поиск по кадру 1920x1200 занимает десятки секунд).
fn scouts_first(mut pixels: Vec<OpaquePixel>) -> Vec<OpaquePixel> {
    if pixels.len() <= PRESCREEN_PIXELS * 2 {
        return pixels;
    }
    let mut mean = [0_u32; 3];
    for pixel in &pixels {
        for (sum, channel) in mean.iter_mut().zip(pixel.color) {
            *sum += u32::from(channel);
        }
    }
    let count = pixels.len() as u32;
    let mean = mean.map(|sum| (sum / count) as u8);
    let distance = |pixel: &OpaquePixel| -> u32 {
        pixel
            .color
            .iter()
            .zip(mean)
            .map(|(channel, mean_channel)| u32::from(channel.abs_diff(mean_channel)))
            .sum()
    };
    pixels.sort_by_key(|pixel| std::cmp::Reverse(distance(pixel)));
    let bright_count = PRESCREEN_PIXELS / 2;
    let step = ((pixels.len() - bright_count) / (PRESCREEN_PIXELS / 2)).max(1);
    let mut ordered: Vec<OpaquePixel> = pixels[..bright_count].to_vec();
    let mut is_scout = vec![false; pixels.len()];
    is_scout[..bright_count].fill(true);
    for index in (bright_count..pixels.len())
        .step_by(step)
        .take(PRESCREEN_PIXELS / 2)
    {
        ordered.push(pixels[index]);
        is_scout[index] = true;
    }
    ordered.extend(
        pixels
            .iter()
            .zip(&is_scout)
            .filter(|(_, scout)| !**scout)
            .map(|(pixel, _)| *pixel),
    );
    ordered
}

/// Горизонтальная полоса кадра, в которой ищем (панель браузера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBand {
    /// Верхняя строка полосы.
    pub top: u32,
    /// Высота полосы.
    pub height: u32,
}

/// Найденный значок.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconMatch {
    /// Левый верхний угол значка.
    pub top_left: Point,
    /// Размер значка на экране (квадрат).
    pub size: u32,
    /// Средняя разница каналов с шаблоном (меньше — точнее).
    pub mean_difference: f32,
}

impl IconMatch {
    /// Центр значка: сюда надо кликать.
    pub fn center(&self) -> Point {
        Point::new(
            self.top_left.x + self.size / 2,
            self.top_left.y + self.size / 2,
        )
    }

    /// Пересекаются ли две находки (одна и та же иконка, найденная со сдвигом или другим масштабом).
    fn overlaps(&self, other: &Self) -> bool {
        let separate_x = self.top_left.x + self.size <= other.top_left.x
            || other.top_left.x + other.size <= self.top_left.x;
        let separate_y = self.top_left.y + self.size <= other.top_left.y
            || other.top_left.y + other.size <= self.top_left.y;
        !(separate_x || separate_y)
    }
}

/// Ищет единственный значок в полосе кадра.
///
/// - нет подходящих мест → [`ScreenError::IconNotFound`] с лучшей (худшей по схожести) разницей;
/// - два НЕпересекающихся места → [`ScreenError::IconAmbiguous`]: кликать наугад нельзя;
/// - иначе — лучшее место (среди пересекающихся находок побеждает с меньшей разницей).
pub fn find_icon(
    frame: &Frame,
    templates: &[IconTemplate],
    band: SearchBand,
    max_mean_difference: f32,
) -> Result<IconMatch, ScreenError> {
    let mut candidates: Vec<IconMatch> = Vec::new();
    let mut closest_difference = f32::INFINITY;
    let band_end = (band.top + band.height).min(frame.height());
    for template in templates {
        if template.size > frame.width() || band.top + template.size > band_end {
            continue;
        }
        // Грубый проход: разведку считаем только в узлах сетки через `COARSE_STEP`; там, где она
        // прошла, полное сравнение делаем во всех положениях вокруг узла (значок мог встать между узлами).
        for top in (band.top..=band_end - template.size).step_by(COARSE_STEP as usize) {
            for left in (0..=frame.width() - template.size).step_by(COARSE_STEP as usize) {
                if !prescreen_passes(frame, template, left, top, max_mean_difference) {
                    continue;
                }
                for fine_top in top.saturating_sub(COARSE_STEP - 1).max(band.top)
                    ..=(top + COARSE_STEP - 1).min(band_end - template.size)
                {
                    for fine_left in left.saturating_sub(COARSE_STEP - 1)
                        ..=(left + COARSE_STEP - 1).min(frame.width() - template.size)
                    {
                        let difference = mean_difference_at(
                            frame,
                            template,
                            fine_left,
                            fine_top,
                            max_mean_difference,
                        );
                        if let Some(difference) = difference {
                            closest_difference = closest_difference.min(difference);
                            if difference <= max_mean_difference {
                                candidates.push(IconMatch {
                                    top_left: Point::new(fine_left, fine_top),
                                    size: template.size,
                                    mean_difference: difference,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    let distinct = distinct_places(candidates);
    match distinct.as_slice() {
        [] => Err(ScreenError::IconNotFound {
            closest_difference,
            screenshot: None,
        }),
        [only] => Ok(*only),
        [first, second, ..] => Err(ScreenError::IconAmbiguous {
            first: first.top_left,
            second: second.top_left,
            screenshot: None,
        }),
    }
}

/// Оставляет по одной находке на «место»: из пересекающихся — с наименьшей разницей.
fn distinct_places(mut candidates: Vec<IconMatch>) -> Vec<IconMatch> {
    candidates.sort_by(|a, b| a.mean_difference.total_cmp(&b.mean_difference));
    let mut places: Vec<IconMatch> = Vec::new();
    for candidate in candidates {
        if !places.iter().any(|place| place.overlaps(&candidate)) {
            places.push(candidate);
        }
    }
    places
}

/// Шаг сетки грубого прохода, пикселей.
const COARSE_STEP: u32 = 2;

/// Прошло ли положение «разведку»: средняя разница по первым пикселям шаблона не выше порога
/// с запасом [`PRESCREEN_REJECT_FACTOR`]. Дёшево отвергает почти весь кадр.
fn prescreen_passes(
    frame: &Frame,
    template: &IconTemplate,
    left: u32,
    top: u32,
    max_mean_difference: f32,
) -> bool {
    let scouts = &template.opaque_pixels[..PRESCREEN_PIXELS.min(template.opaque_pixels.len())];
    let allowed = max_mean_difference * PRESCREEN_REJECT_FACTOR * (scouts.len() * 3) as f32;
    let mut total = 0.0_f32;
    for pixel in scouts {
        let Some(seen) = frame.pixel(left + pixel.dx, top + pixel.dy) else {
            return false;
        };
        for (seen_channel, expected_channel) in seen.iter().zip(pixel.color) {
            total += f32::from(seen_channel.abs_diff(expected_channel));
        }
        if total > allowed {
            return false;
        }
    }
    true
}

/// Средняя разница каналов в одном положении; `None`, если расчёт прерван, потому что разница
/// уже заведомо выше порога. Ранний выход по порогу делает поиск по кадру быстрым: почти каждое
/// положение отвергается по первым пикселям (по «лучшей из виденных» прерывать нельзя: пока совпадений
/// нет, эта граница слишком высока, и кадр считался бы почти целиком, десятки секунд).
fn mean_difference_at(
    frame: &Frame,
    template: &IconTemplate,
    left: u32,
    top: u32,
    max_mean_difference: f32,
) -> Option<f32> {
    let channel_count = (template.opaque_pixels.len() * 3) as f32;
    let abort_above = max_mean_difference * channel_count;
    let mut total = 0.0_f32;
    for (checked, pixel) in template
        .opaque_pixels
        .iter()
        .enumerate()
        .map(|(i, p)| (i + 1, p))
    {
        let seen = frame.pixel(left + pixel.dx, top + pixel.dy)?;
        for (seen_channel, expected_channel) in seen.iter().zip(pixel.color) {
            total += f32::from(seen_channel.abs_diff(expected_channel));
        }
        if total > abort_above {
            return None;
        }
        if checked == PRESCREEN_PIXELS
            && total > max_mean_difference * PRESCREEN_REJECT_FACTOR * (PRESCREEN_PIXELS * 3) as f32
        {
            return None;
        }
    }
    Some(total / channel_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Синтетический значок: тёмный квадрат с яркой диагональю и светлым центром, по краю
    /// прозрачная рамка (как скруглённые углы настоящего значка).
    fn synthetic_icon(size: u32) -> RgbaImage {
        let mut rgba = Vec::new();
        for y in 0..size {
            for x in 0..size {
                let on_border = x == 0 || y == 0 || x == size - 1 || y == size - 1;
                let pixel = if on_border {
                    [0, 0, 0, 0]
                } else if x == y {
                    [250, 120, 20, 255]
                } else if x > size / 3 && x < 2 * size / 3 && y > size / 3 && y < 2 * size / 3 {
                    [240, 240, 240, 255]
                } else {
                    [30, 30, 40, 255]
                };
                rgba.extend_from_slice(&pixel);
            }
        }
        RgbaImage {
            width: size,
            height: size,
            rgba,
        }
    }

    fn templates() -> Vec<IconTemplate> {
        vec![IconTemplate::from_image(&synthetic_icon(20)).expect("rich enough template")]
    }

    const BAND: SearchBand = SearchBand { top: 0, height: 60 };

    /// Панель браузера: светло-серый фон.
    fn toolbar() -> Frame {
        Frame::filled(300, 100, [235, 235, 238])
    }

    #[test]
    fn finds_the_icon_and_reports_its_center() {
        let mut frame = toolbar();
        frame.paint(&synthetic_icon(20), 200, 12);
        let found = find_icon(&frame, &templates(), BAND, DEFAULT_MAX_MEAN_DIFFERENCE)
            .expect("icon is on the toolbar");
        assert_eq!(found.top_left, Point::new(200, 12));
        assert_eq!(found.center(), Point::new(210, 22));
        assert!(found.mean_difference < 1.0, "{}", found.mean_difference);
    }

    #[test]
    fn missing_icon_is_an_error_not_a_guess() {
        let result = find_icon(&toolbar(), &templates(), BAND, DEFAULT_MAX_MEAN_DIFFERENCE);
        assert!(
            matches!(result, Err(ScreenError::IconNotFound { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn icon_outside_the_band_is_not_found() {
        let mut frame = toolbar();
        frame.paint(&synthetic_icon(20), 100, 70);
        let result = find_icon(&frame, &templates(), BAND, DEFAULT_MAX_MEAN_DIFFERENCE);
        assert!(
            matches!(result, Err(ScreenError::IconNotFound { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn two_copies_are_ambiguous() {
        let mut frame = toolbar();
        frame.paint(&synthetic_icon(20), 40, 10);
        frame.paint(&synthetic_icon(20), 220, 10);
        let result = find_icon(&frame, &templates(), BAND, DEFAULT_MAX_MEAN_DIFFERENCE);
        match result {
            Err(ScreenError::IconAmbiguous { first, second, .. }) => {
                let mut places = [first, second];
                places.sort_by_key(|point| point.x);
                assert_eq!(places, [Point::new(40, 10), Point::new(220, 10)]);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_similar_but_different_icon_does_not_count() {
        // Тот же рисунок другого цвета: «похожий» значок соседнего экстеншена.
        let mut decoy = synthetic_icon(20);
        for pixel in decoy.rgba.as_chunks_mut::<4>().0 {
            if pixel[3] == 255 {
                pixel[0] = 255 - pixel[0];
                pixel[1] = 255 - pixel[1];
                pixel[2] = 255 - pixel[2];
            }
        }
        let mut frame = toolbar();
        frame.paint(&synthetic_icon(20), 200, 12);
        frame.paint(&decoy, 60, 12);
        let found = find_icon(&frame, &templates(), BAND, DEFAULT_MAX_MEAN_DIFFERENCE)
            .expect("only the real icon matches");
        assert_eq!(found.top_left, Point::new(200, 12));
    }

    #[test]
    fn icon_scaled_by_the_browser_is_found_with_a_matching_template() {
        let mut frame = toolbar();
        frame.paint(&synthetic_icon(28), 120, 8);
        let both_sizes = vec![
            IconTemplate::from_image(&synthetic_icon(20)).expect("template"),
            IconTemplate::from_image(&synthetic_icon(28)).expect("template"),
        ];
        let found = find_icon(&frame, &both_sizes, BAND, DEFAULT_MAX_MEAN_DIFFERENCE)
            .expect("found at the drawn size");
        assert_eq!((found.top_left, found.size), (Point::new(120, 8), 28));
    }

    #[test]
    fn flat_template_is_rejected_as_too_poor() {
        let flat = RgbaImage {
            width: 4,
            height: 4,
            rgba: vec![255; 4 * 4 * 4],
        };
        assert!(IconTemplate::from_image(&flat).is_none());
    }
}
