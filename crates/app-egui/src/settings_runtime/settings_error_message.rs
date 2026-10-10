//! Русские формулировки ошибок настроек для панели статуса и подсказок под полями.
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
            "Это значение не подходит для данной настройки".to_string()
        }
        SettingValueError::OutOfRange { min, max, .. } => {
            format!("Значение должно быть от {min} до {max}")
        }
        SettingValueError::UnknownStaticOption { .. } => "Выбран неизвестный вариант".to_string(),
        SettingValueError::InvalidVectorLength { .. } => {
            "Указано неверное количество значений".to_string()
        }
        SettingValueError::InvalidListLength { .. } => {
            "В списке неверное количество элементов".to_string()
        }
        SettingValueError::DuplicateListOption { .. } => {
            "В списке есть повторяющийся вариант".to_string()
        }
        SettingValueError::InvalidTextLength { .. } => "Недопустимая длина текста".to_string(),
        SettingValueError::InvalidAutoFixedPositiveInteger { .. } => {
            "Введите положительное целое число или выберите автоматический режим".to_string()
        }
    }
}

/// Фраза про ошибку работы со списком настроек; название настройки берётся из registry.
pub(super) fn settings_error_text(labels: &SettingLabels<'_>, error: &SettingsError) -> String {
    match error {
        SettingsError::DuplicateSettingId { .. } => {
            "Внутренняя ошибка: список настроек собран некорректно".to_string()
        }
        SettingsError::UnknownSetting { .. } => "Неизвестная настройка".to_string(),
        SettingsError::ReadOnlySetting { id } => {
            format!("{} нельзя изменить", labels.quoted(id))
        }
        SettingsError::ResetUnavailable { id } => {
            format!(
                "{} нельзя сбросить к значению по умолчанию",
                labels.quoted(id)
            )
        }
        SettingsError::InvalidValue { id, reason } => {
            format!(
                "{}: {}",
                labels.quoted(id),
                setting_value_error_text(reason)
            )
        }
        SettingsError::AccessFailed { id: Some(id), .. } => {
            format!("Не удалось прочитать или записать {}", labels.quoted(id))
        }
        SettingsError::AccessFailed { id: None, .. } => {
            "Не удалось прочитать или записать настройки".to_string()
        }
    }
}
