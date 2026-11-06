//! Где элементы страницы на ЭКРАНЕ: калибровка настоящей мышью.
//!
//! Страница знает свои элементы в клиентских координатах окна, а робот кликает по экрану. Где
//! окно браузера стоит на экране, странице неизвестно (Wayland это скрывает). Поэтому робот ставит
//! курсор в известную точку экрана, страница сообщает, какими `clientX/clientY` это пришло
//! (строки `entry-probe pointer` из `site/lib/entry-probe.js`), и смещение = экран − клиент.
//! Здесь только разбор строк и арифметика; движение мыши и ожидание журнала — в `robot`/`menu_rows`.

use std::collections::HashMap;

use crate::desktop_input::Point;

/// Метка строки зонда про движение мыши в верхнем документе.
pub(super) const POINTER_LINE_MARK: &str = "entry-probe pointer top";
/// Метка строки зонда про `contextmenu` в верхнем документе.
pub(super) const CONTEXT_MENU_LINE_MARK: &str = "entry-probe contextmenu top";

/// Прямоугольник элемента в клиентских координатах окна страницы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ClientRect {
    pub(super) left: f64,
    pub(super) top: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

/// Одно показание зонда: куда пришёл курсор и где на тот момент лежали цели.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PointerSample {
    pub(super) client_x: f64,
    pub(super) client_y: f64,
    /// Размер прокручиваемой области документа.
    pub(super) scroll_size: (f64, f64),
    pub(super) rects: HashMap<String, ClientRect>,
}

/// Разбирает `entry-probe pointer top client=700,600 inner=1500x900 scroll=1500x900 rects=video:10,60,480,270;...`.
pub(super) fn parse_pointer_line(line: &str) -> Result<PointerSample, String> {
    let after_mark = line
        .split_once(POINTER_LINE_MARK)
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("not a pointer line of the page probe: {line}"))?;
    let client = field(after_mark, "client=")
        .ok_or_else(|| format!("no client= in the probe line: {line}"))?;
    let (client_x, client_y) = parse_pair(client)
        .ok_or_else(|| format!("cannot read client coordinates '{client}' in: {line}"))?;
    let mut rects = HashMap::new();
    if let Some(list) = field(after_mark, "rects=") {
        for item in list.split(';').filter(|item| !item.is_empty()) {
            let (name, numbers) = item
                .rsplit_once(':')
                .ok_or_else(|| format!("bad rect '{item}' in: {line}"))?;
            let rect = parse_rect(numbers)
                .ok_or_else(|| format!("cannot read rect numbers '{numbers}' in: {line}"))?;
            rects.insert(name.to_owned(), rect);
        }
    }
    let scroll = field(after_mark, "scroll=")
        .ok_or_else(|| format!("no scroll= in the probe line: {line}"))?;
    let scroll_size = scroll
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
        .ok_or_else(|| format!("cannot read scroll size '{scroll}' in: {line}"))?;
    Ok(PointerSample {
        client_x,
        client_y,
        scroll_size,
        rects,
    })
}

/// Значение поля `name=` до следующего пробела (или до конца строки).
fn field<'line>(text: &'line str, name: &str) -> Option<&'line str> {
    let start = text.find(name)? + name.len();
    let rest = &text[start..];
    Some(rest.split(' ').next().unwrap_or(rest))
}

fn parse_pair(text: &str) -> Option<(f64, f64)> {
    let (first, second) = text.split_once(',')?;
    Some((first.parse().ok()?, second.parse().ok()?))
}

fn parse_rect(text: &str) -> Option<ClientRect> {
    let numbers: Vec<f64> = text
        .split(',')
        .map(|number| number.parse().ok())
        .collect::<Option<_>>()?;
    match numbers.as_slice() {
        [left, top, width, height] => Some(ClientRect {
            left: *left,
            top: *top,
            width: *width,
            height: *height,
        }),
        _ => None,
    }
}

/// Откуда на экран: смещение окна страницы и прямоугольники целей на момент калибровки.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ScreenPlace {
    offset_x: f64,
    offset_y: f64,
    rects: HashMap<String, ClientRect>,
    scroll_size: (f64, f64),
}

/// Допуск расхождения смещений двух калибровочных точек, px. Масштаб 1 даёт нулевое расхождение;
/// запас на округление страницы до десятых.
const OFFSET_TOLERANCE_PX: f64 = 1.0;

impl ScreenPlace {
    /// Строит по двум показаниям: курсор стоял в экранных точках `first_at` и `second_at`.
    /// Смещения обязаны совпасть: иначе масштаб экрана не 1 или окно двигалось между точками.
    pub(super) fn calibrate(
        first_at: Point,
        first: &PointerSample,
        second_at: Point,
        second: &PointerSample,
    ) -> Result<Self, String> {
        let offset_of = |at: Point, sample: &PointerSample| {
            (
                f64::from(at.x) - sample.client_x,
                f64::from(at.y) - sample.client_y,
            )
        };
        let (first_x, first_y) = offset_of(first_at, first);
        let (second_x, second_y) = offset_of(second_at, second);
        if (first_x - second_x).abs() > OFFSET_TOLERANCE_PX
            || (first_y - second_y).abs() > OFFSET_TOLERANCE_PX
        {
            return Err(format!(
                "the two calibration points disagree: offset ({first_x}, {first_y}) vs ({second_x}, {second_y}); the screen scale is not 1 or the window moved"
            ));
        }
        Ok(Self {
            offset_x: second_x,
            offset_y: second_y,
            rects: second.rects.clone(),
            scroll_size: second.scroll_size,
        })
    }

    /// Точка на экране внутри цели `target`: `share_x`/`share_y` — доли ширины и высоты (0.5 — центр).
    pub(super) fn point_in(
        &self,
        target: &str,
        share_x: f64,
        share_y: f64,
    ) -> Result<Point, String> {
        let rect = self.rects.get(target).ok_or_else(|| {
            format!(
                "the page did not report the target '{target}' (reported: {:?})",
                self.rects.keys().collect::<Vec<_>>()
            )
        })?;
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return Err(format!("the target '{target}' has an empty rect: {rect:?}"));
        }
        let x = self.offset_x + rect.left + rect.width * share_x;
        let y = self.offset_y + rect.top + rect.height * share_y;
        if x < 0.0 || y < 0.0 {
            return Err(format!("the target '{target}' is off screen at ({x}, {y})"));
        }
        // Координаты экрана неотрицательны (проверено выше) и помещаются в u32.
        Ok(Point::new(x.round() as u32, y.round() as u32))
    }

    /// Прямоугольник цели на экране (целые пиксели, обрезанный слева и сверху по краю экрана).
    pub(super) fn screen_rect(&self, target: &str) -> Option<super::native_menu::Rect> {
        let rect = self.rects.get(target)?;
        let edge = |value: f64| value.max(0.0).round() as u32;
        Some(super::native_menu::Rect {
            left: edge(self.offset_x + rect.left),
            top: edge(self.offset_y + rect.top),
            right: edge(self.offset_x + rect.left + rect.width),
            bottom: edge(self.offset_y + rect.top + rect.height),
        })
    }

    /// Прямоугольник цели в клиентских координатах (для сверки вёрстки до и после).
    pub(super) fn client_rect(&self, target: &str) -> Option<ClientRect> {
        self.rects.get(target).copied()
    }

    /// Размер прокручиваемой области документа на момент калибровки.
    pub(super) fn scroll_size(&self) -> (f64, f64) {
        self.scroll_size
    }

    /// Точка экрана по клиентским координатам окна страницы.
    pub(super) fn point_from_client(&self, client_x: f64, client_y: f64) -> Point {
        Point::new(
            (self.offset_x + client_x).max(0.0).round() as u32,
            (self.offset_y + client_y).max(0.0).round() as u32,
        )
    }
}

/// Куда вернулся `contextmenu` на странице: клиентские координаты и кто принял событие.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContextMenuSeen {
    pub(super) target: String,
    pub(super) prevented: bool,
}

/// Разбирает `entry-probe contextmenu top client=.. target=video#v prevented=false`.
pub(super) fn parse_context_menu_line(line: &str) -> Result<ContextMenuSeen, String> {
    let after_mark = line
        .split_once(CONTEXT_MENU_LINE_MARK)
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("not a contextmenu line of the page probe: {line}"))?;
    let target =
        field(after_mark, "target=").ok_or_else(|| format!("no target= in the line: {line}"))?;
    let prevented = match field(after_mark, "prevented=") {
        Some("true") => true,
        Some("false") => false,
        other => return Err(format!("bad prevented={other:?} in: {line}")),
    };
    Ok(ContextMenuSeen {
        target: target.to_owned(),
        prevented,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE_A: &str = "mse-fmp4: entry-probe pointer top client=700,480 inner=1500x900 scroll=1500x900 rects=video:16,60,480,270;iframe:10,10,5,5";
    const LINE_B: &str = "mse-fmp4: entry-probe pointer top client=760,540 inner=1500x900 scroll=1500x900 rects=video:16,60,480,270";

    #[test]
    fn a_pointer_line_gives_client_coordinates_and_every_rect() {
        let sample = parse_pointer_line(LINE_A).expect("parses");
        assert_eq!((sample.client_x, sample.client_y), (700.0, 480.0));
        assert_eq!(sample.scroll_size, (1500.0, 900.0));
        assert_eq!(sample.rects.len(), 2);
        assert_eq!(
            sample.rects["video"],
            ClientRect {
                left: 16.0,
                top: 60.0,
                width: 480.0,
                height: 270.0
            }
        );
    }

    #[test]
    fn a_target_name_with_a_colon_keeps_the_whole_name() {
        let line =
            "entry-probe pointer top client=1,2 inner=3x4 scroll=3x4 rects=video:second:1,2,3,4";
        let sample = parse_pointer_line(line).expect("parses");
        assert!(sample.rects.contains_key("video:second"));
    }

    #[test]
    fn garbage_lines_are_errors_not_zeros() {
        assert!(parse_pointer_line("hello").is_err());
        assert!(parse_pointer_line("entry-probe pointer top inner=1x1").is_err());
        assert!(
            parse_pointer_line("entry-probe pointer top client=a,b scroll=1x1 rects=").is_err()
        );
        assert!(
            parse_pointer_line("entry-probe pointer top client=1,2 scroll=1x1 rects=video:1,2,3")
                .is_err()
        );
    }

    #[test]
    fn calibration_maps_a_target_to_the_screen_and_checks_scale() {
        let first = parse_pointer_line(LINE_A).expect("a");
        let second = parse_pointer_line(LINE_B).expect("b");
        // Окно начинается на экране в (100, 120): точки (800, 600) и (860, 660).
        let place =
            ScreenPlace::calibrate(Point::new(800, 600), &first, Point::new(860, 660), &second)
                .expect("calibrated");
        // Центр видео: client (16 + 240, 60 + 135) = (256, 195) + смещение (100, 120).
        assert_eq!(place.point_in("video", 0.5, 0.5), Ok(Point::new(356, 315)));
        // Масштаб 2: те же клиентские координаты при вдвое большем шаге точек не сходятся.
        let scaled =
            ScreenPlace::calibrate(Point::new(800, 600), &first, Point::new(920, 780), &second);
        assert!(scaled.is_err());
    }

    #[test]
    fn an_unreported_or_empty_target_is_an_error_naming_it() {
        let first = parse_pointer_line(LINE_A).expect("a");
        let second = parse_pointer_line(LINE_B).expect("b");
        let place =
            ScreenPlace::calibrate(Point::new(800, 600), &first, Point::new(860, 660), &second)
                .expect("calibrated");
        let error = place.point_in("shadow-card", 0.5, 0.5).expect_err("absent");
        assert!(error.contains("shadow-card"), "{error}");
    }

    #[test]
    fn a_contextmenu_line_says_who_took_the_event() {
        let seen = parse_context_menu_line(
            "p: entry-probe contextmenu top client=1,2 target=video#v prevented=true",
        )
        .expect("parses");
        assert_eq!(
            seen,
            ContextMenuSeen {
                target: "video#v".to_owned(),
                prevented: true
            }
        );
        assert!(
            parse_context_menu_line("entry-probe contextmenu top client=1,2 target=x").is_err()
        );
    }
}
