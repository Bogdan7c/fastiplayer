//! Пользовательский стиль субтитров (настройки плеера) и его именованные пресеты.
//!
//! [`SubtitleAppearance`] — то, что пользователь меняет в настройках: размер шрифта, цвет,
//! вид подложки или обводки, поля. Все размеры заданы ОТНОСИТЕЛЬНО (доля холста или доля
//! размера шрифта), поэтому один и тот же стиль одинаково выглядит на любом разрешении:
//! пиксели считаются только в момент рисования (`to_event_style`).
//!
//! Значения пресетов — не конфиг, а документированные константы стиля. Пользовательские
//! значения придут из конфига в сессии 04 и будут собираться через `new`-конструкторы
//! валидированных типов (недопустимое значение — ошибка, а не молчаливая подмена).
//!
//! ## Единица «em»
//! Везде, где написано «em», имеется в виду размер шрифта в смысле libass — «высота ячейки»
//! (`ascent + descent` лица), то есть `FontSizeFraction * высота холста`. Для Noto Sans
//! это примерно 1,36 «типографского» em; выбор единицы документирован здесь, чтобы
//! значения пресетов читались однозначно.

mod fraction;
mod placement;
mod to_event_style;

pub use fraction::{CanvasFraction, EmSize, FontSizeFraction, InvalidFraction, LineWidthFraction};
pub(crate) use to_event_style::{CollisionDirection, PlateStyle, cue_style};

use subtitle_core::SubtitleColor;

/// Плашка позади каждой строки реплики (стиль «как YouTube»).
///
/// Это расширение Fastiplayer, а не `BorderStyle` 3/4 из libass: плашка — скруглённый
/// прямоугольник вокруг каждой строки, а все строки реплики сливаются в одну маску.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoxDecoration {
    /// Цвет плашки (альфа задаёт прозрачность всей плашки).
    pub color: SubtitleColor,
    /// Отступ плотной рамки букв строки до края плашки по горизонтали, в em.
    pub horizontal_padding_em: EmSize,
    /// То же по вертикали: от верха/низа ячейки строки до края плашки, в em.
    pub vertical_padding_em: EmSize,
    /// Радиус скругления углов, в em.
    pub corner_radius_em: EmSize,
}

/// Обводка и тень вокруг букв (классический стиль субтитров).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutlineShadowDecoration {
    /// Цвет обводки.
    pub outline_color: SubtitleColor,
    /// Толщина обводки, в em.
    pub outline_width_em: EmSize,
    /// Цвет тени.
    pub shadow_color: SubtitleColor,
    /// Смещение тени вправо и вниз, в em.
    pub shadow_offset_em: EmSize,
}

/// Чем выделен текст на фоне видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextDecoration {
    /// Скруглённая плашка за каждой строкой.
    BackgroundBox(BoxDecoration),
    /// Обводка и тень.
    OutlineAndShadow(OutlineShadowDecoration),
}

/// Пользовательский стиль субтитров.
///
/// Реализует `Eq + Hash`: стиль входит в ключ кэша готовых реплик (изменил настройку —
/// реплики перерисовываются, не изменил — берутся из кэша).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubtitleAppearance {
    /// Размер шрифта как доля высоты холста («высота ячейки» libass).
    pub font_size: FontSizeFraction,
    /// Цвет текста; авторский цвет спана (`SpanStyle.color`) приоритетнее.
    pub text_color: SubtitleColor,
    /// Подложка либо обводка с тенью.
    pub decoration: TextDecoration,
    /// Расстояние от нижнего края реплики до низа холста, доля высоты холста.
    pub bottom_margin: CanvasFraction,
    /// Расстояние от боковых краёв блока до краёв холста, доля ширины холста.
    pub side_margin: CanvasFraction,
    /// Максимальная ширина строки как доля ширины холста (перенос строк по ней).
    pub max_line_width: LineWidthFraction,
}

impl SubtitleAppearance {
    /// Размер шрифта пресетов: 1/18 высоты холста.
    ///
    /// Обоснование: на кадре 720p ячейка Noto Sans получается 40 px, то есть кегль около
    /// 29 px — это размер субтитров YouTube при масштабе «100%» на 720p и привычный для
    /// фильмов 5–6 % высоты кадра для ячейки.
    pub const PRESET_FONT_SIZE: f32 = 1.0 / 18.0;

    /// Нижний отступ пресетов: 6 % высоты холста (у YouTube плашка стоит чуть выше
    /// панели управления, около 5–7 %).
    pub const PRESET_BOTTOM_MARGIN: f32 = 0.06;

    /// Боковой отступ пресетов: 5 % ширины.
    pub const PRESET_SIDE_MARGIN: f32 = 0.05;

    /// Максимальная ширина строки пресетов: 90 % ширины холста.
    pub const PRESET_MAX_LINE_WIDTH: f32 = 0.9;

    /// Стиль «как YouTube»: белый текст на тёмной плашке rgba(8, 8, 8, 0.75) за каждой строкой.
    ///
    /// Параметры плашки подобраны по виду YouTube: заметный горизонтальный отступ
    /// (0,25 em ≈ 10 px на 720p), небольшой вертикальный (0,06 em ≈ 2–3 px) и скругление
    /// 0,15 em (≈ 6 px). Альфа 191 = 0,75 · 255 с округлением.
    pub fn youtube_like() -> Self {
        Self {
            font_size: FontSizeFraction::preset(Self::PRESET_FONT_SIZE),
            text_color: SubtitleColor::opaque(255, 255, 255),
            decoration: TextDecoration::BackgroundBox(BoxDecoration {
                color: SubtitleColor {
                    red: 8,
                    green: 8,
                    blue: 8,
                    alpha: 191,
                },
                horizontal_padding_em: EmSize::preset(0.25),
                vertical_padding_em: EmSize::preset(0.06),
                corner_radius_em: EmSize::preset(0.15),
            }),
            bottom_margin: CanvasFraction::preset(Self::PRESET_BOTTOM_MARGIN),
            side_margin: CanvasFraction::preset(Self::PRESET_SIDE_MARGIN),
            max_line_width: LineWidthFraction::preset(Self::PRESET_MAX_LINE_WIDTH),
        }
    }

    /// Классический стиль: белый текст, чёрная обводка 0,06 em (≈ 2,4 px на 720p) и мягкая
    /// чёрная тень (60 % непрозрачности) со смещением 0,04 em (≈ 1,6 px).
    pub fn outline_and_shadow() -> Self {
        Self {
            font_size: FontSizeFraction::preset(Self::PRESET_FONT_SIZE),
            text_color: SubtitleColor::opaque(255, 255, 255),
            decoration: TextDecoration::OutlineAndShadow(OutlineShadowDecoration {
                outline_color: SubtitleColor::opaque(0, 0, 0),
                outline_width_em: EmSize::preset(0.06),
                shadow_color: SubtitleColor {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 153,
                },
                shadow_offset_em: EmSize::preset(0.04),
            }),
            bottom_margin: CanvasFraction::preset(Self::PRESET_BOTTOM_MARGIN),
            side_margin: CanvasFraction::preset(Self::PRESET_SIDE_MARGIN),
            max_line_width: LineWidthFraction::preset(Self::PRESET_MAX_LINE_WIDTH),
        }
    }
}

#[cfg(test)]
mod tests;
