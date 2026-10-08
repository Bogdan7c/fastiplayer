//! Validation секции `[ui]`: язык, skin, настройки окна, сайдбара и анимаций.

use crate::{AppConfig, ConfigResult};

use super::{invalid_value, validate_u16_range};

/// Единственный skin, для которого текущий UI гарантирует layout contract.
pub(crate) const DEFAULT_UI_SKIN: &str = "minimal";

/// Минимальная частота live preview: ноль означал бы выключенный pacing, а не валидную частоту.
pub(crate) const MIN_LIVE_PREVIEW_MAX_HZ: u16 = 1;

/// Верхняя граница live preview защищает runtime от слишком частых preview updates.
pub(crate) const MAX_LIVE_PREVIEW_MAX_HZ: u16 = 240;

/// Нижняя граница времени анимации sidebar: ноль валиден и означает «без анимации».
pub(crate) const MIN_SIDEBAR_SLIDE_DURATION_MS: u16 = 0;

/// Верхняя граница времени анимации sidebar: дольше 5 секунд UI ощущается сломанным.
pub(crate) const MAX_SIDEBAR_SLIDE_DURATION_MS: u16 = 5000;

/// Минимальная высота кастомного titlebar: ниже кнопки окна становятся слишком мелкими.
pub(crate) const MIN_TITLEBAR_HEIGHT_PX: u16 = 32;

/// Максимальная высота кастомного titlebar: выше этого overlay начинает занимать слишком много видео.
pub(crate) const MAX_TITLEBAR_HEIGHT_PX: u16 = 96;

/// Минимальная длина кода языка UI.
pub(crate) const MIN_UI_LANGUAGE_LEN: usize = 1;

/// Максимальная длина кода языка UI.
pub(crate) const MAX_UI_LANGUAGE_LEN: usize = 16;

/// Проверяет UI section.
pub(super) fn validate_ui_section(config: &AppConfig) -> ConfigResult<()> {
    let language = config.ui.language.trim();
    if language.chars().count() < MIN_UI_LANGUAGE_LEN {
        return Err(invalid_value(
            "ui.language",
            "язык UI не должен быть пустым".to_string(),
        ));
    }

    if language.chars().count() > MAX_UI_LANGUAGE_LEN {
        return Err(invalid_value(
            "ui.language",
            "язык UI должен быть коротким кодом, например `ru` или `en`".to_string(),
        ));
    }

    if config.ui.skin.trim() != DEFAULT_UI_SKIN {
        return Err(invalid_value(
            "ui.skin",
            format!(
                "неизвестный skin `{}`; поддерживается только `{DEFAULT_UI_SKIN}`",
                config.ui.skin
            ),
        ));
    }

    validate_u16_range(
        "ui.settings.live_preview_max_hz",
        config.ui.settings.live_preview_max_hz,
        MIN_LIVE_PREVIEW_MAX_HZ,
        MAX_LIVE_PREVIEW_MAX_HZ,
    )?;

    validate_u16_range(
        "ui.sidebar.width_points",
        config.ui.sidebar.width_points,
        crate::MIN_SIDEBAR_WIDTH_POINTS,
        crate::MAX_SIDEBAR_WIDTH_POINTS,
    )?;

    validate_u16_range(
        "ui.animations.sidebar_slide_duration_ms",
        config.ui.animations.sidebar_slide_duration_ms,
        MIN_SIDEBAR_SLIDE_DURATION_MS,
        MAX_SIDEBAR_SLIDE_DURATION_MS,
    )?;

    validate_u16_range(
        "ui.window.titlebar_height_px",
        config.ui.window.titlebar_height_px,
        MIN_TITLEBAR_HEIGHT_PX,
        MAX_TITLEBAR_HEIGHT_PX,
    )?;

    validate_u16_range(
        "ui.window.corner_radius_px",
        config.ui.window.corner_radius_px,
        crate::MIN_WINDOW_CORNER_RADIUS_PX,
        crate::MAX_WINDOW_CORNER_RADIUS_PX,
    )?;

    Ok(())
}
