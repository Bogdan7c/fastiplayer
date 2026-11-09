//! Прогноз «каким рендером будет рисовать окно» без создания окна (сессия 13a моста).
//!
//! Процессу браузерного режима нужно знать рендер раньше, чем появится окно: метка
//! «дешёвых» кодеков уходит экстеншену в `hello`, а окно создаётся только на первом
//! сеансе. Профиль `Auto` сам по себе ответа не даёт — на машине без аппаратного Vulkan
//! окно молча встанет на GLES2 (решение 40 плана gles2-renderer), и сайту надо говорить
//! «только H.264» (решение 43 моста).
//!
//! Прогноз — та же чистая выборка [`select_renderer`], с теми же пробами каталога и той же
//! меткой незавершённого прошлого старта, что у окна. Поэтому прогноз и окно расходятся
//! только если машина изменилась между ними (драйвер обновили на ходу) или создание
//! рендера окна упало и сработал фолбэк — это редкий случай, и он делает метку лишь
//! неточной, а не опасной.
//!
//! Модуль метку не пишет и не снимает: как и окно браузерного режима, он только читает
//! её (`ReadOnlyRendererStartupMarker`). Остатки проб (instance Vulkan) освобождаются
//! сразу после прогноза: окно позже пробует заново со своими остатками.

use fastiplayer_config::RenderProfile;
use render_shell_api::{RendererFamily, RendererProbe};

use super::selection::{RendererSelection, RendererStartupMarker, select_renderer};
use super::{ProbeLeftovers, platform_candidates};

/// Чем, по прогнозу, будет рисовать окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowRendererForecast {
    /// Окно встанет на это семейство.
    Family(RendererFamily),

    /// Годного рендера нет: окно не откроется вовсе.
    NoUsableRenderer,
}

/// Прогнозирует рендер окна по профилю настроек и метке прошлого старта.
///
/// Пробы настоящие (поднимают Vulkan instance или EGL-контекст) и ленивые: на машине с
/// годным первым кандидатом проба второго не выполняется. Вызывать один раз на процесс.
pub(crate) fn forecast_window_renderer(
    profile: RenderProfile,
    startup_marker: RendererStartupMarker,
) -> WindowRendererForecast {
    let catalog_order: Vec<RendererFamily> = platform_candidates()
        .iter()
        .map(|candidate| candidate.family)
        .collect();
    // Остатки живут только на время прогноза; окно потом пробует со своими.
    let mut probe_leftovers = ProbeLeftovers::default();
    forecast_with_probe(profile, &catalog_order, startup_marker, &mut |family| {
        probe_platform_candidate(family, &mut probe_leftovers)
    })
}

/// Проба кандидата каталога платформы по семейству.
fn probe_platform_candidate(
    family: RendererFamily,
    probe_leftovers: &mut ProbeLeftovers,
) -> RendererProbe {
    match platform_candidates()
        .iter()
        .find(|candidate| candidate.family == family)
    {
        Some(candidate) => (candidate.probe)(probe_leftovers),
        // Семейство берётся из того же каталога, поэтому сюда не попадает; честное «нет»
        // вместо паники.
        None => RendererProbe::Unusable {
            reason: render_shell_api::RendererUnusableReason::NoHardwareAdapter,
        },
    }
}

/// Прогноз поверх произвольной пробы (в тестах — подставной, без GPU).
fn forecast_with_probe(
    profile: RenderProfile,
    catalog_order: &[RendererFamily],
    startup_marker: RendererStartupMarker,
    probe_family: &mut dyn FnMut(RendererFamily) -> RendererProbe,
) -> WindowRendererForecast {
    match select_renderer(profile, catalog_order, startup_marker, probe_family) {
        RendererSelection::Create { family, .. } => WindowRendererForecast::Family(family),
        RendererSelection::NoUsableRenderer { .. } => WindowRendererForecast::NoUsableRenderer,
    }
}

#[cfg(test)]
mod tests {
    use render_shell_api::{RendererFamily, RendererProbe, RendererUnusableReason};

    use super::{
        RenderProfile, RendererStartupMarker, WindowRendererForecast, forecast_with_probe,
    };

    const CATALOG: [RendererFamily; 2] = [RendererFamily::WgpuVulkan, RendererFamily::Gles2];

    /// Проба, где годны только перечисленные семейства; считает вызовы.
    fn probe_where_usable(
        usable: &[RendererFamily],
        calls: &mut Vec<RendererFamily>,
    ) -> impl FnMut(RendererFamily) -> RendererProbe {
        let usable = usable.to_vec();
        move |family| {
            calls.push(family);
            if usable.contains(&family) {
                RendererProbe::Usable
            } else {
                RendererProbe::Unusable {
                    reason: RendererUnusableReason::NoHardwareAdapter,
                }
            }
        }
    }

    fn forecast(
        profile: RenderProfile,
        marker: RendererStartupMarker,
        usable: &[RendererFamily],
    ) -> (WindowRendererForecast, Vec<RendererFamily>) {
        let mut calls = Vec::new();
        let result = forecast_with_probe(
            profile,
            &CATALOG,
            marker,
            &mut probe_where_usable(usable, &mut calls),
        );
        (result, calls)
    }

    /// Главная дыра 09b: Auto на машине без аппаратного Vulkan — окно на GLES2.
    #[test]
    fn auto_without_hardware_vulkan_forecasts_gles2() {
        let (result, _) = forecast(
            RenderProfile::Auto,
            RendererStartupMarker::NoUnfinishedStartup,
            &[RendererFamily::Gles2],
        );
        assert_eq!(
            result,
            WindowRendererForecast::Family(RendererFamily::Gles2)
        );
    }

    /// Auto с годным Vulkan — wgpu, и проба GLES2 не тратится.
    #[test]
    fn auto_with_hardware_vulkan_forecasts_wgpu_without_probing_gles2() {
        let (result, calls) = forecast(
            RenderProfile::Auto,
            RendererStartupMarker::NoUnfinishedStartup,
            &CATALOG,
        );
        assert_eq!(
            result,
            WindowRendererForecast::Family(RendererFamily::WgpuVulkan)
        );
        assert_eq!(calls, vec![RendererFamily::WgpuVulkan]);
    }

    /// Обычный плеер упал на Vulkan до первого кадра — окно моста его тоже обойдёт.
    #[test]
    fn auto_respects_the_crash_marker_of_the_regular_player() {
        let (result, _) = forecast(
            RenderProfile::Auto,
            RendererStartupMarker::UnfinishedStartup(RendererFamily::WgpuVulkan),
            &CATALOG,
        );
        assert_eq!(
            result,
            WindowRendererForecast::Family(RendererFamily::Gles2)
        );
    }

    /// Явный GLES2, который на машине негоден, — окно встанет на wgpu, и прогноз это видит.
    #[test]
    fn explicit_profile_that_is_unusable_forecasts_the_fallback() {
        let (result, _) = forecast(
            RenderProfile::OpenGles,
            RendererStartupMarker::NoUnfinishedStartup,
            &[RendererFamily::WgpuVulkan],
        );
        assert_eq!(
            result,
            WindowRendererForecast::Family(RendererFamily::WgpuVulkan)
        );
    }

    #[test]
    fn nothing_usable_is_reported_as_such() {
        let (result, _) = forecast(
            RenderProfile::Auto,
            RendererStartupMarker::NoUnfinishedStartup,
            &[],
        );
        assert_eq!(result, WindowRendererForecast::NoUsableRenderer);
    }
}
