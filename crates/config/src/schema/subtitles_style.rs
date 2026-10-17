//! Диапазоны и значения по умолчанию для языков, размера текста, цветов и положения субтитров
//! (секция `[subtitles]`). Вынесены из `subtitles.rs` без изменений, чтобы файл схемы секции
//! не раздувался; публичный путь констант прежний - `pub use` в `schema.rs`.

/// Минимальная длина списка языков (пустой список = язык системы).
pub const MIN_PREFERRED_LANGUAGES_LEN: usize = 0;
/// Максимальная длина строки со списком языков.
pub const MAX_PREFERRED_LANGUAGES_LEN: usize = 120;
/// Сколько языков можно перечислить.
pub const MAX_PREFERRED_LANGUAGE_COUNT: usize = 8;

/// Размер текста по умолчанию, % (100 = стандартный пресет).
pub const DEFAULT_TEXT_SIZE_PERCENT: u16 = 100;
/// Минимальный размер текста, %.
pub const MIN_TEXT_SIZE_PERCENT: u16 = 50;
/// Максимальный размер текста, %.
pub const MAX_TEXT_SIZE_PERCENT: u16 = 250;

/// Длина цвета `#RRGGBB` в символах.
pub const HEX_COLOR_LEN: usize = 7;
/// Цвет текста по умолчанию.
pub const DEFAULT_TEXT_COLOR: &str = "#FFFFFF";
/// Цвет плашки по умолчанию: совпадает с `SubtitleAppearance::youtube_like()` (8, 8, 8).
pub const DEFAULT_BACKGROUND_COLOR: &str = "#080808";

/// Непрозрачность плашки по умолчанию: альфа 191 из 255 = 75 %, как в `youtube_like()`.
pub const DEFAULT_BACKGROUND_OPACITY_PERCENT: u8 = 75;
/// Минимальная непрозрачность плашки (полностью прозрачная).
pub const MIN_BACKGROUND_OPACITY_PERCENT: u8 = 0;
/// Максимальная непрозрачность плашки.
pub const MAX_BACKGROUND_OPACITY_PERCENT: u8 = 100;

/// Положение по горизонтали по умолчанию: центр картинки, %.
pub const DEFAULT_POSITION_HORIZONTAL_PERCENT: u8 = 50;
/// Минимум положения по горизонтали (левый край картинки), %.
pub const MIN_POSITION_HORIZONTAL_PERCENT: u8 = 0;
/// Максимум положения по горизонтали (правый край картинки), %.
pub const MAX_POSITION_HORIZONTAL_PERCENT: u8 = 100;

/// Положение по вертикали по умолчанию: низ текста на 6 % высоты картинки выше её низа.
///
/// ВАЖНО: число обязано равняться нижнему отступу пресета растеризатора
/// (`CanvasFraction` 0.06 в `subtitle-raster`): приложение считает смещение как
/// `value/100 - отступ_пресета`, и при значении по умолчанию оно ровно нулевое.
/// Это закреплено тестом в `app-egui`.
pub const DEFAULT_POSITION_VERTICAL_PERCENT: i16 = 6;
/// Минимум положения по вертикали: отрицательное значение уводит текст на чёрную полосу под картинкой, %.
pub const MIN_POSITION_VERTICAL_PERCENT: i16 = -40;
/// Максимум положения по вертикали (почти верх картинки), %.
pub const MAX_POSITION_VERTICAL_PERCENT: i16 = 95;
