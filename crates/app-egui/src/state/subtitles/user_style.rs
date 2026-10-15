//! Пользовательский стиль субтитров как одно значение: позиция, размер, цвета, оформление.
//!
//! Единый источник правды для внешнего вида (решение владельца, субтитры, сессия 05):
//! - `from_config` читает секцию `[subtitles]` (и при этом не падает на неверных значениях);
//! - `to_appearance` переводит стиль в `SubtitleAppearance` растеризатора;
//! - `commit_requests` отдаёт значения для записи обратно в `config.toml`;
//! - `vertical_offset_fraction` / `horizontal_offset_fraction` описывают сдвиг слоя
//!   относительно стандартного места пресета (по умолчанию сдвиг ровно нулевой).
//!
//! Правила перевода в `SubtitleAppearance` (решение владельца, сессия 04):
//! - основа: `decoration = box` -> `youtube_like()`, `outline` -> `outline_and_shadow()`;
//! - `text_size_percent` умножает `PRESET_FONT_SIZE` (100 % = пресет);
//! - `text_color` задаёт цвет текста; для плашки `background_color` + `background_opacity_percent`.
//!
//! Конфиг уже проверен `validate()`, но код не полагается на это вслепую: непонятное
//! значение не роняет приложение, а заменяется значением по умолчанию с записью в лог.

use fastiplayer_config::{
    DEFAULT_BACKGROUND_COLOR, DEFAULT_POSITION_HORIZONTAL_PERCENT,
    DEFAULT_POSITION_VERTICAL_PERCENT, DEFAULT_TEXT_COLOR, MAX_POSITION_HORIZONTAL_PERCENT,
    MAX_POSITION_VERTICAL_PERCENT, MIN_POSITION_VERTICAL_PERCENT, SubtitleDecorationPreference,
    SubtitlesConfig,
};
use subtitle_core::SubtitleColor;
use subtitle_raster::{FontSizeFraction, SubtitleAppearance, TextDecoration};

/// Максимум канала альфы и процентов для пересчёта «проценты -> 0..=255».
const ALPHA_MAX: u32 = 255;
const PERCENT_MAX: u32 = 100;

/// Цвет без прозрачности: прозрачность плашки хранится отдельно (в процентах).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubtitleRgb {
    /// Красный канал.
    pub(crate) red: u8,
    /// Зелёный канал.
    pub(crate) green: u8,
    /// Синий канал.
    pub(crate) blue: u8,
}

impl SubtitleRgb {
    /// Разбирает `#RRGGBB`; ошибка пишется в лог, возвращается `None`.
    fn parse_config_text(text: &str, setting_name: &str) -> Option<Self> {
        match SubtitleColor::from_hex_rgb_str(text) {
            Ok(color) => Some(Self {
                red: color.red,
                green: color.green,
                blue: color.blue,
            }),
            Err(error) => {
                tracing::warn!(%error, setting_name, "Subtitle color is invalid; using the default");
                None
            }
        }
    }

    /// Цвет растеризатора с заданной альфой.
    fn with_alpha(self, alpha: u8) -> SubtitleColor {
        SubtitleColor {
            red: self.red,
            green: self.green,
            blue: self.blue,
            alpha,
        }
    }
}

/// Место слоя субтитров: центр по горизонтали и высота низа текста по вертикали.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubtitlePosition {
    /// Центр слоя, % ширины картинки (0 = левый край, 100 = правый).
    pub(crate) horizontal_percent: u8,
    /// Низ текста над низом картинки, % высоты (минус = на полосе под картинкой).
    pub(crate) vertical_percent: i16,
}

impl SubtitlePosition {
    /// Позиция из конфига; значение вне диапазона заменяется значением по умолчанию с записью в лог.
    fn from_config(config: &SubtitlesConfig) -> Self {
        let horizontal_percent =
            if config.position_horizontal_percent <= MAX_POSITION_HORIZONTAL_PERCENT {
                config.position_horizontal_percent
            } else {
                tracing::warn!(
                    value = config.position_horizontal_percent,
                    "Subtitle horizontal position is out of range; using the default"
                );
                DEFAULT_POSITION_HORIZONTAL_PERCENT
            };
        let vertical_percent = if (MIN_POSITION_VERTICAL_PERCENT..=MAX_POSITION_VERTICAL_PERCENT)
            .contains(&config.position_vertical_percent)
        {
            config.position_vertical_percent
        } else {
            tracing::warn!(
                value = config.position_vertical_percent,
                "Subtitle vertical position is out of range; using the default"
            );
            DEFAULT_POSITION_VERTICAL_PERCENT
        };
        Self {
            horizontal_percent,
            vertical_percent,
        }
    }
}

/// Стиль субтитров, который выбирает пользователь (настройки и режим правки на видео).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserSubtitleStyle {
    /// Где стоит слой субтитров.
    pub(crate) position: SubtitlePosition,
    /// Размер текста, % от пресета (проверяется при переводе в `SubtitleAppearance`).
    pub(crate) text_size_percent: u16,
    /// Цвет текста.
    pub(crate) text_color: SubtitleRgb,
    /// Цвет плашки (без прозрачности).
    pub(crate) background_color: SubtitleRgb,
    /// Непрозрачность плашки, %.
    pub(crate) background_opacity_percent: u8,
    /// Плашка или обводка.
    pub(crate) decoration: SubtitleDecorationPreference,
    /// Применять стиль и к ASS/SSA. Пока ASS-рендера нет, на показ не влияет:
    /// значение только хранится и записывается в конфиг.
    pub(crate) ass_force_user_style: bool,
}

impl Default for UserSubtitleStyle {
    /// Стиль из значений по умолчанию конфига (= пресет `youtube_like()`).
    fn default() -> Self {
        Self::from_config(&SubtitlesConfig::default())
    }
}

impl UserSubtitleStyle {
    /// Читает стиль из секции конфига; неверные значения заменяются значениями по умолчанию.
    pub(crate) fn from_config(config: &SubtitlesConfig) -> Self {
        Self {
            position: SubtitlePosition::from_config(config),
            text_size_percent: config.text_size_percent,
            text_color: SubtitleRgb::parse_config_text(&config.text_color, "text_color")
                .unwrap_or_else(|| default_color(DEFAULT_TEXT_COLOR)),
            background_color: SubtitleRgb::parse_config_text(
                &config.background_color,
                "background_color",
            )
            .unwrap_or_else(|| default_color(DEFAULT_BACKGROUND_COLOR)),
            background_opacity_percent: config.background_opacity_percent,
            decoration: config.decoration,
            ass_force_user_style: config.ass_force_user_style,
        }
    }

    /// Стиль растеризатора. Позиция сюда не входит: слой двигается на GPU целиком.
    pub(crate) fn to_appearance(&self) -> SubtitleAppearance {
        let mut appearance = match self.decoration {
            SubtitleDecorationPreference::Box => SubtitleAppearance::youtube_like(),
            SubtitleDecorationPreference::Outline => SubtitleAppearance::outline_and_shadow(),
        };
        appearance.font_size = scaled_font_size(self.text_size_percent, appearance.font_size);
        appearance.text_color = self.text_color.with_alpha(u8::MAX);
        if let TextDecoration::BackgroundBox(plate) = &mut appearance.decoration {
            plate.color = self
                .background_color
                .with_alpha(alpha_from_percent(self.background_opacity_percent));
        }
        appearance
    }
}

/// Цвет-константа из конфига по умолчанию; неразбираемая константа дала бы чёрный (недостижимо: тест).
fn default_color(config_text: &str) -> SubtitleRgb {
    SubtitleRgb::parse_config_text(config_text, "default").unwrap_or(SubtitleRgb {
        red: 0,
        green: 0,
        blue: 0,
    })
}

/// Размер шрифта пресета, умноженный на процент; вне допустимого диапазона — пресет.
fn scaled_font_size(percent: u16, preset: FontSizeFraction) -> FontSizeFraction {
    // Множитель считается первым: при 100 % он ровно 1.0, и размер пресета остаётся побитово тем же.
    let scale = f32::from(percent) / 100.0;
    let scaled = SubtitleAppearance::PRESET_FONT_SIZE * scale;
    FontSizeFraction::new(scaled).unwrap_or_else(|error| {
        tracing::warn!(%error, "Subtitle text size is out of range; using the default size");
        preset
    })
}

/// Проценты непрозрачности -> альфа 0..=255 с округлением (75 % -> 191, как в `youtube_like`).
fn alpha_from_percent(percent: u8) -> u8 {
    let percent = u32::from(percent).min(PERCENT_MAX);
    let alpha = (percent * ALPHA_MAX + PERCENT_MAX / 2) / PERCENT_MAX;
    u8::try_from(alpha).unwrap_or(u8::MAX)
}

// Сдвиг слоя - размещением (`layer_placement`), запись стиля - сохранением из режима правки.
mod commit;
mod offset;

pub(super) use self::offset::{
    horizontal_offset_fraction_for_percent, position_percent_for_offset_fractions,
    vertical_offset_fraction_for_percent,
};

#[cfg(test)]
mod tests;
