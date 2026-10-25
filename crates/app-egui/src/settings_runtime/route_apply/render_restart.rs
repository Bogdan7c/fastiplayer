//! Разделение committed render route на «пересоздание рендера» и «применится после перезапуска».
//!
//! Один маршрут `RenderCommitted` несёт настройки двух разных судеб: `render.tone_mapping` и
//! `[render.vulkan]` меняют живой рендер, а `render.profile` (выбор семейства рендера) только
//! сохраняется и вступает в силу после перезапуска приложения (решение владельца, сессия 09,
//! №37). Здесь решается, какой исход и какой механизм попадут в отчёт маршрута.

use fastiplayer_settings::{AppRouteApplyResult, AppRuntimeRouteGroup, AppRuntimeRouteGroupUpdate};
use settings_core::ApplyMechanism;

/// Итог маршрута: результат живой части (пересоздание) + механизм для отчёта.
pub(super) struct RenderCommittedOutcome {
    /// Результат на уровне всего маршрута.
    pub(super) result: AppRouteApplyResult,

    /// Механизм, о котором пользователю сообщит строка статуса.
    pub(super) mechanism: ApplyMechanism,
}

/// Собирает итог маршрута из результата пересоздания и состава групп.
///
/// Если в маршруте ТОЛЬКО настройки «после перезапуска», живой рендер не трогался, и это
/// явный исход `AppliesAfterRestart` (не `Noop`: значение действительно принято). Если есть
/// и живые настройки, исход маршрута — результат пересоздания: он описывает единственную
/// рискованную операцию; per-group различие сохраняет `group_result`.
pub(super) fn render_committed_outcome(
    groups: &[AppRuntimeRouteGroupUpdate],
    recreation_result: AppRouteApplyResult,
) -> RenderCommittedOutcome {
    let only_restart_bound = !groups.is_empty()
        && groups
            .iter()
            .all(|group| group.group == AppRuntimeRouteGroup::RenderProfileRestart);
    if only_restart_bound && recreation_result.is_success() {
        return RenderCommittedOutcome {
            result: AppRouteApplyResult::AppliesAfterRestart,
            mechanism: ApplyMechanism::AppliesAfterRestart,
        };
    }
    RenderCommittedOutcome {
        result: recreation_result,
        mechanism: ApplyMechanism::RendererRecreate,
    }
}

#[cfg(test)]
mod tests {
    use settings_core::SettingId;

    use super::*;

    fn group(group: AppRuntimeRouteGroup, setting: &str) -> AppRuntimeRouteGroupUpdate {
        AppRuntimeRouteGroupUpdate {
            group,
            affected_settings: vec![SettingId::from(setting)],
        }
    }

    /// Только профиль: успех живой части даёт `AppliesAfterRestart`, а не `Noop`.
    #[test]
    fn profile_only_route_reports_applies_after_restart() {
        let groups = [group(
            AppRuntimeRouteGroup::RenderProfileRestart,
            "render.profile",
        )];

        let outcome = render_committed_outcome(&groups, AppRouteApplyResult::Noop);

        assert_eq!(outcome.result, AppRouteApplyResult::AppliesAfterRestart);
        assert_eq!(outcome.mechanism, ApplyMechanism::AppliesAfterRestart);
    }

    /// Профиль вместе с живой настройкой: исход — результат пересоздания.
    #[test]
    fn mixed_route_keeps_recreation_result_and_mechanism() {
        let groups = [
            group(
                AppRuntimeRouteGroup::RenderBackendLifecycle,
                "render.tone_mapping",
            ),
            group(AppRuntimeRouteGroup::RenderProfileRestart, "render.profile"),
        ];

        let outcome = render_committed_outcome(&groups, AppRouteApplyResult::Applied);

        assert_eq!(outcome.result, AppRouteApplyResult::Applied);
        assert_eq!(outcome.mechanism, ApplyMechanism::RendererRecreate);
    }

    /// Только живые настройки: поведение пересоздания не меняется.
    #[test]
    fn live_only_route_is_unaffected() {
        let groups = [group(
            AppRuntimeRouteGroup::RenderBackendLifecycle,
            "render.vulkan.present_mode",
        )];

        let outcome = render_committed_outcome(&groups, AppRouteApplyResult::Noop);

        assert_eq!(outcome.result, AppRouteApplyResult::Noop);
        assert_eq!(outcome.mechanism, ApplyMechanism::RendererRecreate);
    }

    /// Сбой живой части не маскируется исходом «после перезапуска».
    #[test]
    fn failure_is_never_reported_as_applies_after_restart() {
        let groups = [group(
            AppRuntimeRouteGroup::RenderProfileRestart,
            "render.profile",
        )];
        let failure = AppRouteApplyResult::Failed {
            message: "boom".to_owned(),
        };

        let outcome = render_committed_outcome(&groups, failure.clone());

        assert_eq!(outcome.result, failure);
    }
}
