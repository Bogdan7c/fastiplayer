//! Родное контекстное меню браузера настоящей мышью.
//!
//! Меню рисует сам браузер (в Chromium и Firefox это не страница), поэтому оно видно только на
//! снимке экрана. У пунктов экстеншена браузер рисует значок экстеншена, и стенд находит строку
//! тем же поиском, что и значок на панели (`find_icon`), но в области вокруг точки клика.
//!
//! Открытие меню доказывается пикселями: кадр до правого клика и кадр после отличаются вокруг
//! точки клика. Если пункта экстеншена в открытом меню нет, это ответ «вход не показан», а не
//! ошибка поиска: ему верят только вместе с положительным контролем (то же меню с включённым
//! входом показывало пункт), его добавляет строка матрицы.

use std::time::Duration;

use super::super::context::ScenarioError;
use super::robot::Robot;
use crate::desktop_input::Point;
use crate::screen_capture::{Frame, ScreenError, SearchBand};

/// Сколько даём меню нарисоваться после правого клика.
const MENU_PAINT_WAIT: Duration = Duration::from_millis(600);
/// Верх области поиска пункта не выше этой строки: выше лежат вкладки и панель с таким же значком.
const BROWSER_TOOLBAR_END: u32 = 200;
/// Область поиска начинается не выше чем на столько над точкой клика (меню может раскрыться вверх).
const MENU_SEARCH_ABOVE_CLICK: u32 = 450;
/// Высота области поиска: самое длинное меню Chromium.
const MENU_SEARCH_HEIGHT: u32 = 900;
/// Как далеко от клика влево и вправо ищем меню (самое широкое меню Chromium с подменю).
const MENU_REACH_SIDE: u32 = 500;
/// Как далеко от клика вверх и вниз ищем меню (оно может раскрыться в любую сторону, самое высокое — около 600 px).
const MENU_REACH_VERTICAL: u32 = 700;
/// Канал пикселя должен отличаться не меньше, чем на столько, чтобы пиксель считался изменённым.
const PIXEL_CHANGE_THRESHOLD: u8 = 24;
/// Сколько новых пикселей вокруг клика считаем «нарисовалось меню».
const MIN_PAINTED_PIXELS_FOR_MENU: usize = 4_000;
/// Сколько новых пикселей рядом с меню считаем «раскрылось подменю».
const MIN_PAINTED_PIXELS_FOR_SUBMENU: usize = 1_500;
/// Сколько даём подменю раскрыться после клика по его строке.
const SUBMENU_OPEN_WAIT: Duration = Duration::from_millis(900);
/// На сколько вокруг родительского меню ищем подменю.
const SUBMENU_REACH: u32 = 600;
/// Расстояние от верха габарита подменю (с тенью) до центра его первой строки.
const SUBMENU_FIRST_ITEM_OFFSET: u32 = 26;

/// Чем кончилось открытие меню.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuPick {
    /// Пункт экстеншена найден и по нему кликнули.
    Picked {
        /// Центр строки, по которой кликнули (у подменю — строка внутри подменю).
        row_center: Point,
        /// Сколько пикселей вокруг клика нарисовало меню (доказательство, что оно было).
        painted_pixels: usize,
        /// Браузер сложил пункты экстеншена в подменю «имя экстеншена ▶» (при нескольких пунктах).
        through_submenu: bool,
    },
    /// Меню открылось, но значка экстеншена в нём нет.
    ItemAbsent { painted_pixels: usize },
    /// Меню не открылось: вокруг клика почти ничего не нарисовалось.
    MenuNotOpened { painted_pixels: usize },
}

/// Как браузер показывает пункты экстеншена в меню ЭТОГО клика.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ItemGrouping {
    /// Один пункт: строка со значком и есть пункт.
    SingleItem,
    /// Несколько пунктов (видео и фрейм одновременно): строка «имя ▶» раскрывает подменю.
    Submenu,
}

impl ItemGrouping {
    fn expects_submenu(self) -> bool {
        self == Self::Submenu
    }
}

/// Прямоугольник экрана в пикселях: `[left, right) x [top, bottom)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) left: u32,
    pub(super) top: u32,
    pub(super) right: u32,
    pub(super) bottom: u32,
}

impl Rect {
    fn contains(&self, x: u32, y: u32) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }

    pub(super) fn width(&self) -> u32 {
        self.right - self.left
    }
}

/// То, что НОВОГО нарисовалось между кадрами: число пикселей и их габарит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Painted {
    pub(super) pixels: usize,
    pub(super) bounds: Option<Rect>,
}

fn pixel_changed(first: &Frame, second: &Frame, x: u32, y: u32) -> bool {
    match (first.pixel(x, y), second.pixel(x, y)) {
        (Some(old), Some(new)) => old.iter().zip(new).any(|(old_channel, new_channel)| {
            old_channel.abs_diff(new_channel) >= PIXEL_CHANGE_THRESHOLD
        }),
        _ => false,
    }
}

/// Что нарисовалось в `within` между вторым «спокойным» кадром и кадром `after`, не считая
/// самого шевелящегося: пиксели, менявшиеся уже между двумя кадрами ДО действия (играющее видео,
/// мигание), отбрасываются. Без этого играющее под меню видео выдавало бы себя за меню.
/// `exclude` — область, которую не считаем (например, уже открытое родительское меню).
pub(super) fn painted_since(
    calm_first: &Frame,
    calm_second: &Frame,
    after: &Frame,
    within: Rect,
    exclude: Option<Rect>,
) -> Painted {
    let right = within.right.min(after.width());
    let bottom = within.bottom.min(after.height());
    let mut pixels = 0;
    let mut bounds: Option<Rect> = None;
    for y in within.top..bottom {
        for x in within.left..right {
            if exclude.is_some_and(|area| area.contains(x, y)) {
                continue;
            }
            if pixel_changed(calm_first, calm_second, x, y)
                || !pixel_changed(calm_second, after, x, y)
            {
                continue;
            }
            pixels += 1;
            bounds = Some(match bounds {
                None => Rect {
                    left: x,
                    top: y,
                    right: x + 1,
                    bottom: y + 1,
                },
                Some(old) => Rect {
                    left: old.left.min(x),
                    top: old.top.min(y),
                    right: old.right.max(x + 1),
                    bottom: old.bottom.max(y + 1),
                },
            });
        }
    }
    Painted { pixels, bounds }
}

/// Область, где ищем меню: вокруг точки клика (меню раскрывается вниз-вправо или зеркально).
fn menu_area(click: Point) -> Rect {
    Rect {
        left: click.x.saturating_sub(MENU_REACH_SIDE),
        top: click.y.saturating_sub(MENU_REACH_VERTICAL),
        right: click.x + MENU_REACH_SIDE,
        bottom: click.y + MENU_REACH_VERTICAL,
    }
}

/// Область, где ищем подменю: полоса справа и слева от родительского меню на всю его высоту с запасом.
fn submenu_area(parent: Rect) -> Rect {
    Rect {
        left: parent.left.saturating_sub(SUBMENU_REACH),
        top: parent.top.saturating_sub(SUBMENU_REACH),
        right: parent.right + SUBMENU_REACH,
        bottom: parent.bottom + SUBMENU_REACH,
    }
}

/// Полоса поиска пункта вокруг точки клика (не заходит на панель браузера).
pub(super) fn menu_search_band(click: Point) -> SearchBand {
    let top = click
        .y
        .saturating_sub(MENU_SEARCH_ABOVE_CLICK)
        .max(BROWSER_TOOLBAR_END);
    SearchBand {
        top,
        height: MENU_SEARCH_HEIGHT,
    }
}

/// Правый клик в `click`, поиск пункта экстеншена и клик по нему (через подменю, если браузер
/// сложил пункты экстеншена в него). Меню остаётся открытым, если пункта нет.
///
/// `target` — прямоугольник элемента, по которому кликнули: на нём страница сама рисует отклик на
/// наведение (панель управления видео появляется под курсором), и это не меню. Для вывода «меню
/// открылось» считаются только новые пиксели ВНЕ элемента: меню выше и шире любого видео стенда.
pub(super) fn right_click_and_pick(
    robot: &Robot<'_>,
    click: Point,
    target: Option<Rect>,
    grouping: ItemGrouping,
) -> Result<MenuPick, ScenarioError> {
    let calm_first = robot.capture_frame("menu-calm-1")?;
    let calm_second = robot.capture_frame("menu-calm-2")?;
    robot.right_click_screen(click)?;
    std::thread::sleep(MENU_PAINT_WAIT);
    let open = robot.capture_frame("menu-open")?;
    let proof = painted_since(&calm_first, &calm_second, &open, menu_area(click), target);
    if proof.pixels < MIN_PAINTED_PIXELS_FOR_MENU {
        return Ok(MenuPick::MenuNotOpened {
            painted_pixels: proof.pixels,
        });
    }
    // Габарит меню целиком (вместе с частью над элементом) нужен, чтобы отличить подменю от родителя.
    let menu = painted_since(&calm_first, &calm_second, &open, menu_area(click), None);
    let Some(menu_bounds) = menu.bounds else {
        return Ok(MenuPick::MenuNotOpened {
            painted_pixels: proof.pixels,
        });
    };
    let row = match robot.find_icon_in_frame(&open, menu_search_band(click)) {
        Ok(found) => found.center(),
        Err(ScreenError::IconNotFound { .. }) => {
            return Ok(MenuPick::ItemAbsent {
                painted_pixels: proof.pixels,
            });
        }
        Err(other) => return Err(other.into()),
    };
    if !grouping.expects_submenu() {
        // Одиночный пункт: клик по строке нажимает его. После клика НИЧЕГО не снимаем: снимок (запуск
        // `spectacle`) в момент, когда плеер открывает окно, отбирает у окна плеера фокус.
        robot.click_screen(row)?;
        return Ok(MenuPick::Picked {
            row_center: row,
            painted_pixels: proof.pixels,
            through_submenu: false,
        });
    }
    // Пункты экстеншена сложены в подменю «имя ▶»: клик по строке раскрывает его (Firefox раскрывает
    // подменю только кликом, наведения мало), первый пункт подменю нажимаем вторым кликом.
    let open_again = robot.capture_frame("menu-open-2")?;
    robot.click_screen(row)?;
    std::thread::sleep(SUBMENU_OPEN_WAIT);
    let after_click = robot.capture_frame("menu-after-click")?;
    let submenu = painted_since(
        &open,
        &open_again,
        &after_click,
        submenu_area(menu_bounds),
        Some(menu_bounds),
    );
    let Some(bounds) = submenu
        .bounds
        .filter(|_| submenu.pixels >= MIN_PAINTED_PIXELS_FOR_SUBMENU)
    else {
        return Ok(MenuPick::ItemAbsent {
            painted_pixels: proof.pixels,
        });
    };
    // Первый пункт подменю: пункты идут в порядке создания, «видео» создан первым.
    let first_item = Point::new(
        bounds.left + bounds.width() / 2,
        bounds.top + SUBMENU_FIRST_ITEM_OFFSET,
    );
    robot.click_screen(first_item)?;
    Ok(MenuPick::Picked {
        row_center: first_item,
        painted_pixels: proof.pixels,
        through_submenu: true,
    })
}

/// Что видно в меню, открытом только чтобы посмотреть (без клика).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuLook {
    /// Меню открыто, пункт экстеншена в нём есть.
    ItemShown { painted_pixels: usize },
    /// Меню открыто, пункта экстеншена нет.
    ItemAbsent { painted_pixels: usize },
    /// Меню не открылось.
    MenuNotOpened { painted_pixels: usize },
}

/// Правый клик и поиск пункта экстеншена без клика по нему; меню остаётся открытым.
pub(super) fn right_click_and_look(
    robot: &Robot<'_>,
    click: Point,
    target: Option<Rect>,
) -> Result<MenuLook, ScenarioError> {
    let calm_first = robot.capture_frame("look-calm-1")?;
    let calm_second = robot.capture_frame("look-calm-2")?;
    robot.right_click_screen(click)?;
    std::thread::sleep(MENU_PAINT_WAIT);
    let open = robot.capture_frame("look-open")?;
    let proof = painted_since(&calm_first, &calm_second, &open, menu_area(click), target);
    if proof.pixels < MIN_PAINTED_PIXELS_FOR_MENU {
        return Ok(MenuLook::MenuNotOpened {
            painted_pixels: proof.pixels,
        });
    }
    Ok(
        match robot.find_icon_in_frame(&open, menu_search_band(click)) {
            Ok(_) => MenuLook::ItemShown {
                painted_pixels: proof.pixels,
            },
            Err(ScreenError::IconNotFound { .. }) => MenuLook::ItemAbsent {
                painted_pixels: proof.pixels,
            },
            Err(other) => return Err(other.into()),
        },
    )
}

/// Закрывает открытое меню (Escape), чтобы следующий клик не ушёл в меню.
pub(super) fn dismiss_menu(robot: &Robot<'_>) -> Result<(), ScenarioError> {
    robot.press(crate::desktop_input::Key::Escape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen_capture::RgbaImage;

    fn gray(width: u32, height: u32) -> RgbaImage {
        RgbaImage {
            width,
            height,
            rgba: [60, 60, 60, 255].repeat((width * height) as usize),
        }
    }

    fn whole(frame: &Frame) -> Rect {
        Rect {
            left: 0,
            top: 0,
            right: frame.width(),
            bottom: frame.height(),
        }
    }

    #[test]
    fn a_painted_menu_counts_with_its_bounds_and_an_unchanged_screen_does_not() {
        let calm = Frame::filled(1000, 800, [255, 255, 255]);
        let mut after = calm.clone();
        assert_eq!(
            painted_since(&calm, &calm, &after, whole(&calm), None).pixels,
            0
        );
        after.paint(&gray(200, 300), 500, 400);
        let painted = painted_since(&calm, &calm, &after, whole(&calm), None);
        assert_eq!(painted.pixels, 200 * 300);
        assert_eq!(
            painted.bounds,
            Some(Rect {
                left: 500,
                top: 400,
                right: 700,
                bottom: 700
            })
        );
        // Изменение вне области поиска не считается.
        let far = Rect {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        assert_eq!(painted_since(&calm, &calm, &after, far, None).pixels, 0);
    }

    #[test]
    fn pixels_that_already_moved_before_the_action_are_not_a_menu() {
        // Играющее видео: область, которая менялась между двумя спокойными кадрами.
        let first = Frame::filled(400, 400, [255, 255, 255]);
        let mut second = first.clone();
        second.paint(&gray(100, 100), 100, 100);
        let mut after = second.clone();
        // К «после» видео шевельнулось в той же области ещё раз (другим цветом).
        after.paint(
            &RgbaImage {
                width: 100,
                height: 100,
                rgba: [200, 10, 10, 255].repeat(100 * 100),
            },
            100,
            100,
        );
        assert_eq!(
            painted_since(&first, &second, &after, whole(&first), None).pixels,
            0
        );
    }

    #[test]
    fn an_excluded_parent_menu_is_not_counted_as_a_submenu() {
        let calm = Frame::filled(1000, 800, [255, 255, 255]);
        let mut after = calm.clone();
        after.paint(&gray(200, 300), 100, 100); // родитель
        after.paint(&gray(150, 100), 320, 120); // подменю рядом
        let parent = Rect {
            left: 100,
            top: 100,
            right: 300,
            bottom: 400,
        };
        let sub = painted_since(&calm, &calm, &after, whole(&calm), Some(parent));
        assert_eq!(sub.pixels, 150 * 100);
        assert_eq!(sub.bounds.map(|bounds| bounds.left), Some(320));
    }

    #[test]
    fn the_search_band_never_reaches_the_browser_toolbar() {
        assert_eq!(
            menu_search_band(Point::new(500, 250)).top,
            BROWSER_TOOLBAR_END
        );
        assert_eq!(menu_search_band(Point::new(500, 900)).top, 450);
    }
}
