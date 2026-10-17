//! Публичные настройки рисования ASS: масштаб текста, источник стиля, размер хранения видео.
//!
//! Все типы — маленькие значения с `Eq + Hash`: они входят в ключ кэша готовых ASS-реплик
//! и в подпись кадра, поэтому смена любой настройки перерисовывает ASS (а реплики SRT/WebVTT
//! эти настройки не затрагивают).

use std::hash::{Hash, Hasher};
use std::num::NonZeroU32;

/// Верхняя граница масштаба текста: защита от абсурдных значений конфига (10 = 1000 %).
const MAX_TEXT_SCALE: f32 = 10.0;

/// Не допустимое значение масштаба текста.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("ASS text scale must be finite and in (0, {MAX_TEXT_SCALE}], got {0}")]
pub struct InvalidAssTextScale(pub f32);

/// Масштаб текста ASS пользователем (`1.0` = 100 %).
///
/// Аналог `ass_set_font_scale` libass (`font_size_coeff`): умножает размер шрифта, обводку и
/// тень событий без жёсткого позиционирования (`\pos`, `\move`, `\clip`, `\org`, `\p`,
/// Banner/Scroll) — как libass при выборочном переопределении стиля. Положение событий
/// (поля, `\an`, координаты) масштаб не меняет.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssTextScale(f32);

impl AssTextScale {
    /// 100 %: текст как задал автор.
    pub const IDENTITY: Self = Self(1.0);

    /// Проверенный масштаб: конечный, больше нуля и не больше 10.
    pub fn new(value: f32) -> Result<Self, InvalidAssTextScale> {
        if value.is_finite() && value > 0.0 && value <= MAX_TEXT_SCALE {
            Ok(Self(value))
        } else {
            Err(InvalidAssTextScale(value))
        }
    }

    /// Значение (`1.0` = 100 %).
    pub const fn get(self) -> f32 {
        self.0
    }
}

// Равенство и хеш по битам: значение всегда конечное и положительное (NaN и -0 исключены
// конструктором), поэтому побитовое сравнение совпадает с числовым.
impl Eq for AssTextScale {}

impl Hash for AssTextScale {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// Чей стиль применяется к ASS-репликам.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssStyleSource {
    /// Стиль автора: шрифты, цвета, обводка, тень и теги оформления из файла; пользовательский
    /// масштаб ([`AssTextScale`]) применяется выборочно, как в libass.
    Author,
    /// «Принудительно мой стиль»: шрифт, размер, цвета и вид подложки (плашка или обводка с
    /// тенью) берутся из `SubtitleAppearance`, авторские теги оформления игнорируются.
    /// Положение остаётся авторским (`\pos`, `\an`, поля, слои, столкновения, PlayRes), а
    /// `\b \i \u \s`, переводы строк, `\q` и `\r` работают как обычно. Масштаб
    /// ([`AssTextScale`]) в этом режиме не используется: размер задаёт `SubtitleAppearance`.
    User,
}

/// Размер хранения видео (libass: `ass_set_storage_size`) — пиксельный размер картинки видео.
///
/// По нему libass масштабирует обводку и тень, когда в сценарии нет `LayoutRes`, и
/// определяет растяжение пикселей (`par`). `Unknown` — размер хранения не сообщён.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssStorageSize {
    /// Не известен: обводка и тень масштабируются как PlayRes (поведение libass без вызова
    /// `ass_set_storage_size`).
    Unknown,
    /// Пиксельный размер видео.
    Known {
        /// Ширина в пикселях.
        width: NonZeroU32,
        /// Высота в пикселях.
        height: NonZeroU32,
    },
}

impl AssStorageSize {
    /// Размер из пикселей; нулевая сторона означает «не известен» (как в libass, где
    /// положителен должен быть и тот и другой размер).
    pub fn from_pixels(width: u32, height: u32) -> Self {
        match (NonZeroU32::new(width), NonZeroU32::new(height)) {
            (Some(width), Some(height)) => Self::Known { width, height },
            _ => Self::Unknown,
        }
    }
}

/// Настройки рисования ASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssRenderOptions {
    /// Масштаб текста пользователем.
    pub text_scale: AssTextScale,
    /// Чей стиль применять.
    pub style_source: AssStyleSource,
    /// Размер хранения видео.
    pub storage_size: AssStorageSize,
}

impl Default for AssRenderOptions {
    /// Стиль автора, масштаб 100 %, размер хранения не известен.
    fn default() -> Self {
        Self {
            text_scale: AssTextScale::IDENTITY,
            style_source: AssStyleSource::Author,
            storage_size: AssStorageSize::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn text_scale_rejects_non_positive_and_non_finite_values() {
        assert_eq!(AssTextScale::new(1.5).map(AssTextScale::get), Ok(1.5));
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY, 10.5] {
            assert!(AssTextScale::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn storage_size_with_a_zero_side_is_unknown() {
        assert_eq!(AssStorageSize::from_pixels(0, 720), AssStorageSize::Unknown);
        assert_eq!(
            AssStorageSize::from_pixels(1280, 0),
            AssStorageSize::Unknown
        );
        assert!(matches!(
            AssStorageSize::from_pixels(1280, 720),
            AssStorageSize::Known { .. }
        ));
    }

    #[test]
    fn options_are_distinct_hash_keys() {
        let base = AssRenderOptions::default();
        let scaled = AssRenderOptions {
            text_scale: AssTextScale::new(1.25).expect("scale"),
            ..base
        };
        let user = AssRenderOptions {
            style_source: AssStyleSource::User,
            ..base
        };
        let stored = AssRenderOptions {
            storage_size: AssStorageSize::from_pixels(640, 360),
            ..base
        };
        let keys: HashSet<_> = [base, scaled, user, stored, base].into_iter().collect();
        assert_eq!(keys.len(), 4);
    }
}
