//! Несохранённые изменения общего config-а и правило их дописывания.
//!
//! Если запись не удалась (файл занят, битый, новее…), изменение уже применено и живёт в
//! памяти (решение 135). Store запоминает id таких настроек и при следующей успешной
//! записи дописывает их вместе с новыми изменениями, беря значение из **текущего**
//! документа в памяти: если настройку с тех пор меняли ещё раз, запишется последнее
//! значение.
//!
//! Правило «не затереть соседа». Для каждого несохранённого id store помнит
//! *базу* — значение, которое по нашим сведениям лежало на диске, когда пользователь сделал
//! первое несохранённое изменение (`before` этого изменения). При дописывании под замком:
//! - значение на диске уже равно значению в памяти — писать нечего (например, D1 принял
//!   это же значение от соседа, или сосед записал то же самое);
//! - значение на диске всё ещё равно базе — после нашей неудачи этот ключ никто не трогал,
//!   записываем значение из памяти;
//! - значение на диске другое — после нашей неудачи ключ записала другая копия. Она
//!   записала его позже нашего изменения, а «один ключ у двоих — побеждает последний»
//!   (решение 135), поэтому значение с диска остаётся, а наше несохранённое забывается.
//!   Подтянуть значение соседа в память — дело слежения за файлом (D1).
//!
//! Так дописывание никогда не записывает старое значение поверх более позднего чужого, и
//! отдельный метод «забыть несохранённое для id» при принятии значения соседа не нужен.

use std::collections::BTreeMap;

use fastiplayer_config::AppConfig;
use settings_core::{SettingId, SettingValue, SettingsDiff, SettingsRegistry, SettingsResult};

/// Id несохранённых настроек и значение диска, от которого каждое изменение отталкивалось.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UnsavedSettingChanges {
    /// id → база (значение на диске до первого несохранённого изменения).
    disk_baselines: BTreeMap<SettingId, SettingValue>,
}

impl UnsavedSettingChanges {
    /// Есть ли изменения, которые ещё не попали в файл.
    pub(crate) fn is_empty(&self) -> bool {
        self.disk_baselines.is_empty()
    }

    /// Id несохранённых настроек в стабильном порядке.
    pub(crate) fn setting_ids(&self) -> Vec<SettingId> {
        self.disk_baselines.keys().cloned().collect()
    }

    /// Запоминает изменения неудачной записи.
    ///
    /// База берётся от **первого** несохранённого изменения id: повторные изменения в
    /// памяти не меняют того, что лежит на диске.
    pub(crate) fn remember_failed_write(&mut self, changed_settings: &SettingsDiff) {
        for change in changed_settings.changes() {
            self.disk_baselines
                .entry(change.id.clone())
                .or_insert_with(|| change.before.clone());
        }
    }

    /// Всё записано или разрешено правилом соседа — помнить больше нечего.
    pub(crate) fn forget_all(&mut self) {
        self.disk_baselines.clear();
    }
}

/// Что стало с несохранёнными id при одной записи.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UnsavedOverlay {
    /// Дописаны значениями из памяти.
    pub(crate) written: Vec<SettingId>,
    /// На диске уже было то же значение, что в памяти.
    pub(crate) already_on_disk: Vec<SettingId>,
    /// После нашей неудачи ключ записала другая копия — оставлено её значение.
    pub(crate) kept_from_disk: Vec<SettingId>,
}

/// Вписывает в документ с диска новые изменения транзакции и дописывает несохранённые.
///
/// Новые изменения (`changed_settings`) пишутся значением `after` всегда: пользователь
/// сделал их только что, значит его запись — последняя. Несохранённые id, которые не
/// входят в новые изменения, проходят правило «не затереть соседа» (см. модуль).
pub(crate) fn overlay_changes(
    registry: &SettingsRegistry<AppConfig>,
    disk_config: &mut AppConfig,
    changed_settings: &SettingsDiff,
    unsaved: &UnsavedSettingChanges,
    memory_config: &AppConfig,
) -> SettingsResult<UnsavedOverlay> {
    for change in changed_settings.changes() {
        registry.set_value(disk_config, &change.id, change.after.clone())?;
    }

    let mut overlay = UnsavedOverlay::default();
    for (setting_id, disk_baseline) in &unsaved.disk_baselines {
        let changed_now = changed_settings
            .changes()
            .iter()
            .any(|change| change.id == *setting_id);
        if changed_now {
            overlay.written.push(setting_id.clone());
            continue;
        }
        let disk_value = registry.get_value(disk_config, setting_id)?;
        let memory_value = registry.get_value(memory_config, setting_id)?;
        if disk_value == memory_value {
            overlay.already_on_disk.push(setting_id.clone());
        } else if disk_value == *disk_baseline {
            registry.set_value(disk_config, setting_id, memory_value)?;
            overlay.written.push(setting_id.clone());
        } else {
            overlay.kept_from_disk.push(setting_id.clone());
        }
    }
    Ok(overlay)
}
