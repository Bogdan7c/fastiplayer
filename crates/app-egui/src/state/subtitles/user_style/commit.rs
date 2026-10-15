//! Запись стиля обратно в `config.toml`: список значений настроек для одной транзакции.

use fastiplayer_config::SubtitleDecorationPreference;
use settings_core::{RuntimeSettingCommitRequest, SettingOptionId, SettingValue};

use super::{SubtitleRgb, UserSubtitleStyle};

/// Stable id настроек, в которые пишется стиль (должны совпадать с `SubtitlesConfig`).
const POSITION_HORIZONTAL_SETTING_ID: &str = "subtitles.position_horizontal_percent";
const POSITION_VERTICAL_SETTING_ID: &str = "subtitles.position_vertical_percent";
const TEXT_SIZE_SETTING_ID: &str = "subtitles.text_size_percent";
const TEXT_COLOR_SETTING_ID: &str = "subtitles.text_color";
const BACKGROUND_COLOR_SETTING_ID: &str = "subtitles.background_color";
const BACKGROUND_OPACITY_SETTING_ID: &str = "subtitles.background_opacity_percent";
const DECORATION_SETTING_ID: &str = "subtitles.decoration";
const ASS_FORCE_USER_STYLE_SETTING_ID: &str = "subtitles.ass_force_user_style";

/// Id вариантов выбора `subtitles.decoration` (как `option(id = ...)` в схеме конфига).
const DECORATION_BOX_OPTION_ID: &str = "box";
const DECORATION_OUTLINE_OPTION_ID: &str = "outline";

impl SubtitleRgb {
    /// Запись `#RRGGBB` (заглавные), как в `config.toml`.
    fn to_config_text(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }
}

impl UserSubtitleStyle {
    /// Все поля стиля как значения настроек для одной записи в `config.toml`.
    ///
    /// Порядок и состав фиксированы: каждый вызов отдаёт все восемь настроек стиля,
    /// чтобы запись была атомарной по смыслу («сохранить стиль целиком»).
    pub(crate) fn commit_requests(&self) -> Vec<RuntimeSettingCommitRequest> {
        let decoration_option_id = match self.decoration {
            SubtitleDecorationPreference::Box => DECORATION_BOX_OPTION_ID,
            SubtitleDecorationPreference::Outline => DECORATION_OUTLINE_OPTION_ID,
        };
        vec![
            RuntimeSettingCommitRequest::new(
                POSITION_HORIZONTAL_SETTING_ID,
                SettingValue::Integer(i64::from(self.position.horizontal_percent)),
            ),
            RuntimeSettingCommitRequest::new(
                POSITION_VERTICAL_SETTING_ID,
                SettingValue::Integer(i64::from(self.position.vertical_percent)),
            ),
            RuntimeSettingCommitRequest::new(
                TEXT_SIZE_SETTING_ID,
                SettingValue::Integer(i64::from(self.text_size_percent)),
            ),
            RuntimeSettingCommitRequest::new(
                TEXT_COLOR_SETTING_ID,
                SettingValue::Text(self.text_color.to_config_text()),
            ),
            RuntimeSettingCommitRequest::new(
                BACKGROUND_COLOR_SETTING_ID,
                SettingValue::Text(self.background_color.to_config_text()),
            ),
            RuntimeSettingCommitRequest::new(
                BACKGROUND_OPACITY_SETTING_ID,
                SettingValue::Integer(i64::from(self.background_opacity_percent)),
            ),
            RuntimeSettingCommitRequest::new(
                DECORATION_SETTING_ID,
                SettingValue::Select(SettingOptionId::from(decoration_option_id)),
            ),
            RuntimeSettingCommitRequest::new(
                ASS_FORCE_USER_STYLE_SETTING_ID,
                SettingValue::Bool(self.ass_force_user_style),
            ),
        ]
    }
}
