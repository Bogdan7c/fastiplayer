//! Значения соседа, которые эта копия уже пыталась принять и не смогла.
//!
//! Принятие изменений соседа идёт одной транзакцией на всю разницу файла. Если маршрут
//! отказал (например, пересборка источника не удалась), человеку один раз показывается
//! «не удалось применить», а значения на диске остаются чужими. Раньше «уже разобрано»
//! помнилось только отпечатком файла: любая следующая запись соседа — даже по
//! несвязанному ключу (размер окна, язык) — давала новый отпечаток, та же отказанная пара
//! снова попадала в разницу, транзакция снова падала, и тост повторялся на каждую запись.
//!
//! Поэтому доска помнит отказанные пары (id, значение): пока на диске лежит то же значение,
//! пара в разницу не попадает, и несвязанные изменения соседа принимаются сами по себе.
//! Новое значение того же id соседом — новая попытка; своё значение пользователя или
//! любое другое на диске — пара забывается.

use std::collections::BTreeMap;

use fastiplayer_config::AppConfig;
use settings_core::{
    ApplyReport, ApplyRouteResult, ExternalSettingChange, SettingId, SettingValue,
    SettingsRegistry, SettingsResult,
};

/// Отказанные при принятии значения соседа (id → значение с диска).
#[derive(Debug, Default)]
pub(super) struct RefusedPeerValues {
    values: BTreeMap<SettingId, SettingValue>,
}

impl RefusedPeerValues {
    /// Запоминает пары неудачного принятия.
    ///
    /// Если отчёт называет упавшие маршруты, запоминаются только их настройки: соседние
    /// значения той же пачки не виноваты и примутся со следующей записью соседа. Если
    /// виноватых назвать нельзя (отказ проверки до маршрутов, отказ без отчёта) —
    /// запоминается вся пачка.
    pub(super) fn remember_refused(
        &mut self,
        attempted: &[ExternalSettingChange],
        report: Option<&ApplyReport>,
    ) {
        let failed_settings = report.map(failed_route_settings).unwrap_or_default();
        for change in attempted {
            let blamed = failed_settings.is_empty() || failed_settings.contains(&change.setting_id);
            if blamed {
                self.values
                    .insert(change.setting_id.clone(), change.value.clone());
            }
        }
    }

    /// Забывает пары, значения которых на диске уже другие (сосед записал новое, или
    /// файл переписала эта копия).
    pub(super) fn forget_values_no_longer_on_disk(
        &mut self,
        registry: &SettingsRegistry<AppConfig>,
        disk: &AppConfig,
    ) -> SettingsResult<()> {
        let mut still_on_disk = BTreeMap::new();
        for (setting_id, refused_value) in &self.values {
            if registry.get_value(disk, setting_id)? == *refused_value {
                still_on_disk.insert(setting_id.clone(), refused_value.clone());
            }
        }
        self.values = still_on_disk;
        Ok(())
    }

    /// Разница без уже отказанных пар.
    pub(super) fn without_refused(
        &self,
        changes: Vec<ExternalSettingChange>,
    ) -> Vec<ExternalSettingChange> {
        changes
            .into_iter()
            .filter(|change| self.values.get(&change.setting_id) != Some(&change.value))
            .collect()
    }
}

/// Настройки маршрутов, которые в отчёте завершились неудачей.
fn failed_route_settings(report: &ApplyReport) -> Vec<SettingId> {
    report
        .routes
        .iter()
        .filter(|route| {
            matches!(
                route.result,
                ApplyRouteResult::Failed { .. }
                    | ApplyRouteResult::PartialFailure { .. }
                    | ApplyRouteResult::Conflict { .. }
            )
        })
        .flat_map(|route| route.affected_settings.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use settings_core::{ApplyFinalState, ApplyMechanism, ApplyRouteReport, SettingRouteId};

    use super::*;

    fn change(setting_id: &str, value: i64) -> ExternalSettingChange {
        ExternalSettingChange::new(setting_id, SettingValue::Integer(value))
    }

    fn report_with_failed_route(failed_setting: &str) -> ApplyReport {
        ApplyReport {
            validation: None,
            persistence: None,
            routes: vec![ApplyRouteReport {
                route: SettingRouteId::from("media"),
                result: ApplyRouteResult::Failed {
                    message: "rebuild failed".to_string(),
                },
                mechanism: ApplyMechanism::InPlace,
                affected_settings: vec![SettingId::from(failed_setting)],
            }],
            rollback: Vec::new(),
            changed_settings: settings_core::SettingsDiff::new(),
            final_state: ApplyFinalState::RuntimeApplyFailed,
            conflicts: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Виноват только упавший маршрут: невиновное значение той же пачки не запоминается,
    /// а новое значение отказанного id — снова кандидат.
    #[test]
    fn only_failed_route_values_are_held_back() {
        let mut refused = RefusedPeerValues::default();
        let attempted = vec![
            change("network.read_ahead_mb", 9),
            change("subtitles.text_size_percent", 120),
        ];

        refused.remember_refused(
            &attempted,
            Some(&report_with_failed_route("network.read_ahead_mb")),
        );
        let next = refused.without_refused(vec![
            change("network.read_ahead_mb", 9),
            change("subtitles.text_size_percent", 120),
            change("ui.sidebar.width_points", 450),
        ]);
        let new_value = refused.without_refused(vec![change("network.read_ahead_mb", 10)]);

        let next_ids: Vec<_> = next
            .iter()
            .map(|change| change.setting_id.as_str())
            .collect();
        assert_eq!(
            next_ids,
            vec!["subtitles.text_size_percent", "ui.sidebar.width_points"]
        );
        assert_eq!(new_value.len(), 1, "a new value is a new attempt");
    }

    /// Без отчёта (отказ до маршрутов) запоминается вся пачка.
    #[test]
    fn whole_batch_is_held_back_when_no_route_is_blamed() {
        let mut refused = RefusedPeerValues::default();
        let attempted = vec![
            change("network.read_ahead_mb", 9),
            change("ui.sidebar.width_points", 450),
        ];

        refused.remember_refused(&attempted, None);

        assert!(refused.without_refused(attempted).is_empty());
    }
}
