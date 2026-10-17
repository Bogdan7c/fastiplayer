//! Настройки рисования ASS, которые презентатор берёт из пользовательского стиля.
//!
//! Решение владельца (субтитры, сессия 08):
//! - без `ass_force_user_style` цвета и оформление автора сохраняются, а размер текста
//!   пользователя применяется как масштаб ASS (`AssTextScale`, как `ass_set_font_scale`:
//!   события с жёсткой позицией `\pos` масштаб не меняет — это делает растеризатор);
//! - с флажком весь ASS рисуется стилем пользователя, позиции автора сохраняются.
//!
//! Третья часть `AssRenderOptions` — размер картинки видео (`storage_size`) — зависит от
//! кадра, а не от стиля, поэтому подставляется на каждом кадре отдельно.

use fastiplayer_config::{DEFAULT_TEXT_SIZE_PERCENT, MAX_TEXT_SIZE_PERCENT, MIN_TEXT_SIZE_PERCENT};
use subtitle_raster::{AssRenderOptions, AssStorageSize, AssStyleSource, AssTextScale};

use crate::state::subtitles::user_style::UserSubtitleStyle;

/// Проценты размера текста, при которых масштаб равен единице.
const PERCENT_PER_UNIT_SCALE: f32 = 100.0;

/// Часть настроек ASS, определяемая стилем пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct AssPresentation {
    text_scale: AssTextScale,
    style_source: AssStyleSource,
}

impl AssPresentation {
    /// Настройки из стиля: масштаб из `text_size_percent`, источник стиля из флажка.
    ///
    /// Процент вне допустимого диапазона конфига заменяется на 100 % с записью в лог:
    /// конфиг уже проверен `validate()`, но показ не должен полагаться на это вслепую.
    pub(in crate::state::subtitles) fn from_style(style: &UserSubtitleStyle) -> Self {
        Self {
            text_scale: text_scale_for_percent(style.text_size_percent),
            style_source: if style.ass_force_user_style {
                AssStyleSource::User
            } else {
                AssStyleSource::Author
            },
        }
    }

    /// Полные настройки рисования для кадра с известным размером картинки видео.
    pub(in crate::state::subtitles) fn render_options(
        self,
        storage_size: AssStorageSize,
    ) -> AssRenderOptions {
        AssRenderOptions {
            text_scale: self.text_scale,
            style_source: self.style_source,
            storage_size,
        }
    }
}

impl Default for AssPresentation {
    /// Стиль автора, масштаб 100 %.
    fn default() -> Self {
        Self {
            text_scale: AssTextScale::IDENTITY,
            style_source: AssStyleSource::Author,
        }
    }
}

/// Масштаб `text_size_percent / 100`; вне диапазона конфига — 100 %.
fn text_scale_for_percent(percent: u16) -> AssTextScale {
    if !(MIN_TEXT_SIZE_PERCENT..=MAX_TEXT_SIZE_PERCENT).contains(&percent) {
        tracing::warn!(
            percent,
            default_percent = DEFAULT_TEXT_SIZE_PERCENT,
            "Subtitle text size is out of range for ASS; using the default scale"
        );
        return AssTextScale::IDENTITY;
    }
    AssTextScale::new(f32::from(percent) / PERCENT_PER_UNIT_SCALE).unwrap_or_else(|error| {
        tracing::warn!(%error, "ASS text scale is invalid; using the default scale");
        AssTextScale::IDENTITY
    })
}

#[cfg(test)]
mod tests {
    use fastiplayer_config::SubtitlesConfig;

    use super::*;

    fn style(text_size_percent: u16, force_user_style: bool) -> UserSubtitleStyle {
        UserSubtitleStyle::from_config(&SubtitlesConfig {
            text_size_percent,
            ass_force_user_style: force_user_style,
            ..SubtitlesConfig::default()
        })
    }

    /// Проценты размера становятся масштабом, флажок выбирает источник стиля.
    #[test]
    fn style_maps_to_scale_and_style_source() {
        let author = AssPresentation::from_style(&style(150, false));
        let options = author.render_options(AssStorageSize::Unknown);
        assert!((options.text_scale.get() - 1.5).abs() < f32::EPSILON);
        assert_eq!(options.style_source, AssStyleSource::Author);

        let forced = AssPresentation::from_style(&style(100, true));
        assert_eq!(
            forced.render_options(AssStorageSize::Unknown).style_source,
            AssStyleSource::User
        );
    }

    /// Размер картинки видео попадает в настройки как есть, нулевая сторона — «неизвестно».
    #[test]
    fn storage_size_comes_from_the_frame() {
        let presentation = AssPresentation::default();
        let known = presentation.render_options(AssStorageSize::from_pixels(1920, 1080));
        assert_eq!(known.storage_size, AssStorageSize::from_pixels(1920, 1080));
        assert_ne!(known.storage_size, AssStorageSize::Unknown);
        let unknown = presentation.render_options(AssStorageSize::from_pixels(0, 1080));
        assert_eq!(unknown.storage_size, AssStorageSize::Unknown);
    }

    /// Мусорный процент не ломает показ: масштаб 100 %.
    #[test]
    fn out_of_range_percent_falls_back_to_identity() {
        let presentation = AssPresentation::from_style(&style(0, false));
        assert_eq!(
            presentation
                .render_options(AssStorageSize::Unknown)
                .text_scale,
            AssTextScale::IDENTITY
        );
    }
}
