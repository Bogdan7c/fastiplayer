//! Человеческие названия настроек для панели статуса.
//!
//! Внутренние id (`audio.volume`, route-ы) — стабильные ключи для кода и конфига, пользователю
//! их показывать нельзя. Единственный источник названий — `label` из metadata registry
//! (`SettingDescriptorText`), поэтому статус всегда совпадает с подписями полей в окне настроек.

use fastiplayer_config::AppConfig;
use settings_core::{SettingId, SettingsRegistry};

/// Подпись, если id не найден в registry. Сам id пользователю не показывается (он уходит в лог).
const UNKNOWN_SETTING_LABEL: &str = "неизвестная настройка";

/// Read-only адаптер «id настройки -> русское название» поверх registry.
pub(super) struct SettingLabels<'registry> {
    registry: &'registry SettingsRegistry<AppConfig>,
}

impl<'registry> SettingLabels<'registry> {
    /// Создаёт адаптер над registry, которым владеет `SettingsRuntime`.
    pub(super) fn new(registry: &'registry SettingsRegistry<AppConfig>) -> Self {
        Self { registry }
    }

    /// Название одной настройки в кавычках-«ёлочках».
    ///
    /// Неизвестный id — признак бага в маршрутизации, а не повод показывать id пользователю:
    /// пишем предупреждение в лог и подставляем нейтральное слово.
    pub(super) fn quoted(&self, setting_id: &SettingId) -> String {
        match self.registry.descriptor(setting_id) {
            Some(descriptor) => format!("«{}»", descriptor.text.label.fallback_ru),
            None => {
                tracing::warn!(
                    setting_id = setting_id.as_str(),
                    "В статусе настроек встретился id, которого нет в registry"
                );
                format!("«{UNKNOWN_SETTING_LABEL}»")
            }
        }
    }

    /// Названия нескольких настроек через запятую; повторы убираются, порядок сохраняется.
    pub(super) fn quoted_list(&self, setting_ids: &[SettingId]) -> String {
        let mut labels: Vec<String> = Vec::with_capacity(setting_ids.len());
        for setting_id in setting_ids {
            let label = self.quoted(setting_id);
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels.join(", ")
    }
}
