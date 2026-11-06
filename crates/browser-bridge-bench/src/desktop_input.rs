//! Настоящая мышь и клавиатура невидимого экрана через протокол KDE `org_kde_kwin_fake_input`.
//!
//! Зачем. Попап экстеншена открывается только настоящим кликом человека по значку на панели
//! браузера: из страницы или через CDP его не открыть. Композитор принимает такие события от
//! клиента `fake_input` и доставляет их окнам так же, как события физической мыши.
//!
//! Безопасность. Модуль строится только из [`DesktopEndpoint`], а его выдаёт окружение, созданное
//! на `DisplayTarget::Virtual` с `DesktopInputPolicy::Allowed`. Экран владельца недостижим по типу.
//!
//! Ошибки протокола не глотаются: после каждого запроса делается `roundtrip`, и отказ
//! композитора (протокольная ошибка) возвращается вызывающему.

mod link;

use std::thread::sleep;
use std::time::Duration;

use link::FakeInputLink;
pub use link::InputError;

use crate::environment::DesktopEndpoint;

/// Код кнопки мыши BTN_LEFT из `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;
/// Код правой кнопки мыши (BTN_RIGHT): открывает контекстное меню.
const BTN_RIGHT: u32 = 0x111;
/// Коды клавиш из `linux/input-event-codes.h`.
const KEY_ESC: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_SPACE: u32 = 57;
const KEY_UP: u32 = 103;
const KEY_DOWN: u32 = 108;

/// Пауза между подводом курсора и нажатием: браузер должен успеть обработать вход курсора
/// в кнопку (подсветка, активация элемента), иначе Chromium иногда съедает первый клик.
const HOVER_SETTLE: Duration = Duration::from_millis(120);
/// Пауза между нажатием и отпусканием: нажатие и отпускание в одном кадре композитора
/// браузеры считают дребезгом и не всегда доставляют как клик. 50 мс — короче двойного клика,
/// но дольше кадра.
const PRESS_HOLD: Duration = Duration::from_millis(50);
/// Пауза после действия: даёт интерфейсу браузера дорисоваться перед следующим шагом робота.
const AFTER_ACTION_SETTLE: Duration = Duration::from_millis(150);

/// Точка экрана в пикселях (масштаб экрана 1, начало — левый верхний угол).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// Расстояние от левого края.
    pub x: u32,
    /// Расстояние от верхнего края.
    pub y: u32,
}

impl Point {
    /// Точка по координатам.
    pub fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}

/// Латинская буква на клавиатуре (10d: человек печатает текст в поле страницы).
/// Создаётся только из `a..=z`, поэтому код клавиши всегда существует.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Letter {
    /// Код клавиши Linux (evdev).
    evdev_code: u32,
}

impl Letter {
    /// Коды клавиш `KEY_A..KEY_Z` НЕ идут по алфавиту (раскладка QWERTY), поэтому таблица.
    const EVDEV_CODES: [u32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, // a..m
        49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44, // n..z
    ];

    /// Клавиша буквы; `None` для всего, что не строчная латинская буква.
    pub fn new(character: char) -> Option<Self> {
        let index = (character as u32).checked_sub('a' as u32)?;
        Self::EVDEV_CODES
            .get(usize::try_from(index).ok()?)
            .map(|&evdev_code| Self { evdev_code })
    }
}

/// Клавиша, которую умеет нажимать робот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Escape.
    Escape,
    /// Tab: перейти к следующему элементу.
    Tab,
    /// Enter: нажать выбранное.
    Enter,
    /// Пробел.
    Space,
    /// Стрелка вверх.
    ArrowUp,
    /// Стрелка вниз.
    ArrowDown,
    /// Латинская буква (без Shift: строчная).
    Letter(Letter),
}

impl Key {
    /// Клавиши, которыми набирается слово из строчных латинских букв; любой другой символ — `None`.
    pub fn typing(word: &str) -> Option<Vec<Key>> {
        word.chars()
            .map(|character| Letter::new(character).map(Key::Letter))
            .collect()
    }

    /// Код клавиши Linux (evdev).
    fn evdev_code(self) -> u32 {
        match self {
            Self::Escape => KEY_ESC,
            Self::Tab => KEY_TAB,
            Self::Enter => KEY_ENTER,
            Self::Space => KEY_SPACE,
            Self::ArrowUp => KEY_UP,
            Self::ArrowDown => KEY_DOWN,
            Self::Letter(letter) => letter.evdev_code,
        }
    }
}

/// Соединение с невидимым экраном, из которого берутся мышь и клавиатура.
pub struct DesktopInput {
    link: FakeInputLink,
}

impl DesktopInput {
    /// Подключается к сокету невидимого экрана и представляется композитору.
    pub fn connect(endpoint: &DesktopEndpoint) -> Result<Self, InputError> {
        Ok(Self {
            link: FakeInputLink::connect(&endpoint.wayland_socket())?,
        })
    }

    /// Мышь экрана.
    pub fn mouse(&self) -> Mouse<'_> {
        Mouse { link: &self.link }
    }

    /// Клавиатура экрана.
    pub fn keyboard(&self) -> Keyboard<'_> {
        Keyboard { link: &self.link }
    }
}

/// Мышь: перемещение и клик.
pub struct Mouse<'input> {
    link: &'input FakeInputLink,
}

impl Mouse<'_> {
    /// Переводит курсор в точку (без клика).
    pub fn move_to(&self, point: Point) -> Result<(), InputError> {
        self.link.pointer_move(point.x.into(), point.y.into())
    }

    /// Подводит курсор и кликает левой кнопкой: наведение, пауза, нажатие, пауза, отпускание.
    pub fn click_at(&self, point: Point) -> Result<(), InputError> {
        self.press_button_at(point, BTN_LEFT)
    }

    /// Подводит курсор и кликает ПРАВОЙ кнопкой: открывает контекстное меню браузера.
    pub fn right_click_at(&self, point: Point) -> Result<(), InputError> {
        self.press_button_at(point, BTN_RIGHT)
    }

    /// Общая последовательность клика любой кнопкой (`button_code` — код evdev).
    fn press_button_at(&self, point: Point, button_code: u32) -> Result<(), InputError> {
        self.move_to(point)?;
        sleep(HOVER_SETTLE);
        self.link.button(button_code, ButtonState::Pressed)?;
        sleep(PRESS_HOLD);
        self.link.button(button_code, ButtonState::Released)?;
        sleep(AFTER_ACTION_SETTLE);
        Ok(())
    }
}

/// Клавиатура: нажатие клавиши.
pub struct Keyboard<'input> {
    link: &'input FakeInputLink,
}

impl Keyboard<'_> {
    /// Нажимает и отпускает клавишу.
    pub fn press(&self, key: Key) -> Result<(), InputError> {
        self.link.key(key.evdev_code(), ButtonState::Pressed)?;
        sleep(PRESS_HOLD);
        self.link.key(key.evdev_code(), ButtonState::Released)?;
        sleep(AFTER_ACTION_SETTLE);
        Ok(())
    }
}

/// Состояние кнопки или клавиши в протоколе `fake_input` (0 — отпущена, 1 — нажата).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonState {
    Released,
    Pressed,
}

impl ButtonState {
    fn wire_value(self) -> u32 {
        match self {
            Self::Released => 0,
            Self::Pressed => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes_match_linux_input_header() {
        // Значения из linux/input-event-codes.h: ошибка в коде нажала бы другую клавишу.
        assert_eq!(Key::Tab.evdev_code(), 15);
        assert_eq!(Key::Enter.evdev_code(), 28);
        assert_eq!(Key::Escape.evdev_code(), 1);
        assert_eq!(Key::Space.evdev_code(), 57);
        assert_eq!(Key::ArrowDown.evdev_code(), 108);
        assert_eq!(Key::ArrowUp.evdev_code(), 103);
    }

    #[test]
    fn letters_map_to_their_qwerty_evdev_codes_and_nothing_else_is_a_letter() {
        // KEY_H=35, KEY_E=18, KEY_L=38, KEY_O=24, KEY_Q=16, KEY_Z=44, KEY_A=30, KEY_M=50.
        let codes: Vec<u32> = Key::typing("hello")
            .expect("letters")
            .into_iter()
            .map(Key::evdev_code)
            .collect();
        assert_eq!(codes, vec![35, 18, 38, 38, 24]);
        for (character, code) in [('a', 30), ('m', 50), ('q', 16), ('z', 44)] {
            let letter = Letter::new(character).expect("a letter");
            assert_eq!(Key::Letter(letter).evdev_code(), code);
        }
        assert!(Key::typing("Hello").is_none(), "capitals need Shift");
        assert!(Key::typing("a b").is_none());
        assert!(Letter::new('{').is_none());
    }

    #[test]
    fn mouse_button_codes_match_linux_input_header() {
        // BTN_LEFT = 0x110, BTN_RIGHT = 0x111: перепутанные коды открыли бы не то меню.
        assert_eq!(BTN_LEFT, 0x110);
        assert_eq!(BTN_RIGHT, 0x111);
    }

    #[test]
    fn button_state_wire_values_are_protocol_values() {
        assert_eq!(ButtonState::Released.wire_value(), 0);
        assert_eq!(ButtonState::Pressed.wire_value(), 1);
    }

    #[test]
    fn connect_to_missing_socket_is_an_error() {
        // Нет сокета — понятная ошибка, а не паника и не тихий успех.
        let missing = std::path::Path::new("/nonexistent/wl-none");
        let error = FakeInputLink::connect(missing).err().expect("must fail");
        assert!(matches!(error, InputError::Connect { .. }), "{error}");
    }
}
