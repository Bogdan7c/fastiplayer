//! Английские формулировки ошибок настроек для панели статуса и подсказок под полями.
//!
//! `Display` у `SettingsError`/`SettingValueError` — англоязычный технический текст для логов
//! (с внутренними id). В интерфейс он не попадает: пользователь видит фразу из таблиц ниже,
//! в которых каждый вид ошибки перечислен явно (`match` без `_` — новый вид ошибки не
//! скомпилируется, пока для него не напишут человеческий текст).

use settings_core::{SettingValueError, SettingsError};

use super::setting_labels::SettingLabels;

/// Фраза про ошибку значения поля (без названия настройки: его добавляет вызывающий).
pub(super) fn setting_value_error_text(error: &SettingValueError) -> String {
    match error {
        SettingValueError::DescriptorTypeMismatch { .. }
        | SettingValueError::UnexpectedType { .. } => {
            "This value is unsuitable for this setting".to_string()
        }
        SettingValueError::OutOfRange { min, max, .. } => {
            format!("Value must be between {min} and {max}")
        }
        SettingValueError::UnknownStaticOption { .. } => {
            "An unknown option was selected".to_string()
        }
        SettingValueError::InvalidVectorLength { .. } => "Incorrect number of values".to_string(),
        SettingValueError::InvalidListLength { .. } => "Incorrect number of list items".to_string(),
        SettingValueError::DuplicateListOption { .. } => {
            "The list contains a duplicate option".to_string()
        }
        SettingValueError::InvalidTextLength { .. } => "Invalid text length".to_string(),
        SettingValueError::InvalidAutoFixedPositiveInteger { .. } => {
            "Enter a positive integer or select Automatic".to_string()
        }
    }
}

/// Фраза про ошибку работы со списком настроек; название настройки берётся из registry.
pub(super) fn settings_error_text(labels: &SettingLabels<'_>, error: &SettingsError) -> String {
    match error {
        SettingsError::DuplicateSettingId { .. } => {
            "Internal error: the settings list is invalid".to_string()
        }
        SettingsError::UnknownSetting { .. } => "Unknown setting".to_string(),
        SettingsError::ReadOnlySetting { id } => {
            format!("{} cannot be changed", labels.quoted(id))
        }
        SettingsError::ResetUnavailable { id } => {
            format!("{} cannot be reset to default", labels.quoted(id))
        }
        SettingsError::InvalidValue { id, reason } => {
            format!(
                "{}: {}",
                labels.quoted(id),
                setting_value_error_text(reason)
            )
        }
        SettingsError::AccessFailed { id: Some(id), .. } => {
            format!("Could not read or write {}", labels.quoted(id))
        }
        SettingsError::AccessFailed { id: None, .. } => {
            "Could not read or write settings".to_string()
        }
    }
}
