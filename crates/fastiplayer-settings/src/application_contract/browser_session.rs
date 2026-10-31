//! Строки application matrix для секции `[browser_session]` (сессия 04c моста).
//!
//! Вынесены отдельным модулем: центральный `application_contract.rs` у лимита размера.
//! Две группы с разной семантикой (контракт маршрута — `routing/browser_session.rs`):
//! - окно буфера источника — политика следующего видео со страницы
//!   (`PolicyUpdateInPlace`, эффект на следующем естественном событии);
//! - запоминаемое окно — «запоминаемое состояние» решения 137 (`RememberedStateOnly`):
//!   значение пишется, но живое окно — своё и соседнее — по маршруту не меняется.

use super::{
    AppRuntimeRoute, POLICY_TESTS, SettingApplicationContract, SettingApplyMechanism,
    SettingApplyTestScenario, SettingStateOwner,
};

/// Сценарии запоминаемого состояния: живой runtime не трогается, повтор — no-op,
/// соседние владельцы (обычное окно, источник, плеер) не затрагиваются.
const REMEMBERED_STATE_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::NoActiveResource,
    SettingApplyTestScenario::ActiveResource,
    SettingApplyTestScenario::RepeatedApplyNoop,
    SettingApplyTestScenario::NoUnrelatedOwnerMutation,
];

/// Строка матрицы для ключа браузерного режима; `None` — ключ не из этой секции.
pub(super) fn browser_session_contract(setting_name: &str) -> Option<SettingApplicationContract> {
    let contract = match setting_name {
        "browser_session.buffer_ahead_s" | "browser_session.buffer_behind_s" => {
            SettingApplicationContract::new(
                setting_name,
                AppRuntimeRoute::BrowserSession,
                SettingStateOwner::BrowserSessionSourcePolicy,
                SettingApplyMechanism::PolicyUpdateInPlace,
                POLICY_TESTS,
            )
        }
        "browser_session.window.inner_width_points"
        | "browser_session.window.inner_height_points"
        | "browser_session.window.fullscreen" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::BrowserSession,
            SettingStateOwner::BrowserWindowRememberedState,
            SettingApplyMechanism::RememberedStateOnly,
            REMEMBERED_STATE_TESTS,
        ),
        _ => return None,
    };
    Some(contract)
}

#[cfg(test)]
mod tests {
    use settings_core::SettingId;

    use super::*;
    use crate::setting_application_contract;

    /// Окно буфера — политика следующего видео со страницы, своим владельцем.
    #[test]
    fn buffer_keys_are_next_page_video_policy() {
        for setting_id in [
            "browser_session.buffer_ahead_s",
            "browser_session.buffer_behind_s",
        ] {
            let contract = setting_application_contract(&SettingId::from(setting_id))
                .expect("buffer contract exists");

            assert_eq!(contract.route, AppRuntimeRoute::BrowserSession);
            assert_eq!(
                contract.state_owner,
                SettingStateOwner::BrowserSessionSourcePolicy
            );
            assert_eq!(
                contract.rollback_owner,
                SettingStateOwner::BrowserSessionSourcePolicy
            );
            assert_eq!(
                contract.mechanism,
                SettingApplyMechanism::PolicyUpdateInPlace
            );
            assert!(
                contract
                    .focused_tests
                    .contains(&SettingApplyTestScenario::EffectOnNextNaturalEvent)
            );
        }
    }

    /// Ключи окна — запоминаемое состояние, которое живьём не применяется.
    #[test]
    fn window_keys_are_remembered_state_only() {
        for setting_id in [
            "browser_session.window.inner_width_points",
            "browser_session.window.inner_height_points",
            "browser_session.window.fullscreen",
        ] {
            let contract = setting_application_contract(&SettingId::from(setting_id))
                .expect("window contract exists");

            assert_eq!(contract.route, AppRuntimeRoute::BrowserSession);
            assert_eq!(
                contract.state_owner,
                SettingStateOwner::BrowserWindowRememberedState
            );
            assert_eq!(
                contract.mechanism,
                SettingApplyMechanism::RememberedStateOnly
            );
        }
        assert!(browser_session_contract("browser_session.window.maximized").is_none());
    }
}
