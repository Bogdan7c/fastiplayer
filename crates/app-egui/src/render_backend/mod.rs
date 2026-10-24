//! Каталог рендеров платформы (решение владельца 3 плана `user/gles2-renderer/`).
//!
//! Приложение знает рендер только как `Box<dyn RenderShell>`. Какие рендеры есть на
//! платформе, в каком порядке их предпочитать и как создать конкретный — знает только этот
//! модуль. Это единственное место `app-egui`, где упоминаются конкретные рендеры: добавить
//! GLES2 (сессия 09) или DX10 — значит дописать кандидата в [`platform_candidates`] и его
//! фабрику, не трогая остальное приложение.
//!
//! Создание рендера идёт только отсюда: при старте ([`create_renderer`] после
//! [`select_renderer`]) и при пересоздании внутри семейства (тот же [`create_renderer`]).

mod selection;
mod wgpu_vulkan;

use std::sync::Arc;

use fastiplayer_config::{AppConfig, VulkanConfig};
use render_shell_api::{
    RenderShell, RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe,
    SurfacePresentSettings,
};
use tracing::warn;
use winit::window::Window;

use crate::settings_runtime::InitialRenderSettings;

use selection::{
    CandidateProbe, RendererSelection, RendererSelectionNotice, RendererStartupMarker,
    select_renderer,
};

/// Что нужно фабрике кандидата для создания рендера.
pub(crate) struct RendererCreateRequest<'config> {
    /// Окно, на котором рендер создаёт свою цель показа (и которому шлёт `pre_present_notify`).
    pub(crate) window: Arc<Window>,

    /// Настройки показа Vulkan/wgpu (`[render.vulkan]`): present mode и задержка swapchain.
    pub(crate) vulkan: &'config VulkanConfig,

    /// Какие настройки цвета у рендера сразу после создания.
    pub(crate) initial_color: RendererInitialColor,
}

/// Настройки цвета нового рендера до первого кадра.
pub(crate) enum RendererInitialColor {
    /// Старт приложения: настройки из сохранённого config-а (прежние `set_*`).
    CommittedConfig(InitialRenderSettings),

    /// Пересоздание: умолчания рендера; живой снимок старого рендера переносит
    /// `RendererLifecycleCoordinator` через `commit_live_settings`.
    RendererDefaults,
}

/// Один кандидат каталога платформы.
struct RendererCandidate {
    /// Семейство кандидата.
    family: RendererFamily,

    /// Проверка «годен ли на этой машине» без создания рендера.
    probe: fn() -> RendererProbe,

    /// Фабрика рендера.
    create: fn(RendererCreateRequest<'_>) -> Result<Box<dyn RenderShell>, RendererCreateError>,

    /// Перевод committed настроек показа в нейтральные (тот же, что у фабрики при создании):
    /// им пользуется перенастройка показа на месте (сессия 02a).
    presentation_settings: fn(&VulkanConfig) -> SurfacePresentSettings,
}

/// Кандидаты платформы в порядке предпочтения (сессия 02: только wgpu/Vulkan).
fn platform_candidates() -> &'static [RendererCandidate] {
    const LINUX_CANDIDATES: &[RendererCandidate] = &[RendererCandidate {
        family: RendererFamily::WgpuVulkan,
        probe: wgpu_vulkan::probe,
        create: wgpu_vulkan::create,
        presentation_settings: wgpu_vulkan::surface_present_settings,
    }];
    LINUX_CANDIDATES
}

/// Пробы всех кандидатов платформы для [`select_renderer`].
fn probe_platform_candidates() -> Vec<CandidateProbe> {
    platform_candidates()
        .iter()
        .map(|candidate| CandidateProbe {
            family: candidate.family,
            probe: (candidate.probe)(),
        })
        .collect()
}

/// Создаёт рендер семейства `family` фабрикой каталога.
pub(crate) fn create_renderer(
    family: RendererFamily,
    request: RendererCreateRequest<'_>,
) -> Result<Box<dyn RenderShell>, RendererCreateError> {
    let Some(candidate) = platform_candidates()
        .iter()
        .find(|candidate| candidate.family == family)
    else {
        return Err(RendererCreateError::new(
            RendererCreateStage::ApiConnection,
            format!(
                "renderer family '{}' is not built into this application",
                family.stable_id()
            ),
        ));
    };
    (candidate.create)(request)
}

/// Семейства рендера нет в каталоге этой сборки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RendererFamilyNotBuilt {
    /// Какое семейство просили.
    pub(crate) family: RendererFamily,
}

/// Настройки показа для перенастройки рендера семейства `family` на месте.
///
/// Перевод `[render.vulkan]` → [`SurfacePresentSettings`] — тот же, что фабрика кандидата
/// применяет при создании рендера: дублировать его в пересоздании запрещено.
pub(crate) fn presentation_settings(
    family: RendererFamily,
    vulkan: &VulkanConfig,
) -> Result<SurfacePresentSettings, RendererFamilyNotBuilt> {
    platform_candidates()
        .iter()
        .find(|candidate| candidate.family == family)
        .map(|candidate| (candidate.presentation_settings)(vulkan))
        .ok_or(RendererFamilyNotBuilt { family })
}

/// Выбирает семейство по профилю из config-а и создаёт рендер для старта приложения.
///
/// Сессия 02: метки незавершённого старта ещё нет (сессия 09), поэтому прошлый старт
/// считается завершённым. Уведомление о замене выбора пока пишется только в лог.
pub(crate) fn create_startup_renderer(
    config: &AppConfig,
    window: Arc<Window>,
    initial_render_settings: InitialRenderSettings,
) -> Result<Box<dyn RenderShell>, RendererCreateError> {
    let candidates = probe_platform_candidates();
    let selection = select_renderer(
        config.render.profile,
        &candidates,
        RendererStartupMarker::NoUnfinishedStartup,
    );
    let family = match selection {
        RendererSelection::Create { family, notice } => {
            if let Some(notice) = notice {
                log_selection_notice(family, &notice);
            }
            family
        }
        RendererSelection::NoUsableRenderer { rejected } => {
            return Err(RendererCreateError::new(
                RendererCreateStage::AdapterSelection,
                format!("no usable renderer on this machine: {rejected:?}"),
            ));
        }
    };
    create_renderer(
        family,
        RendererCreateRequest {
            window,
            vulkan: &config.render.vulkan,
            initial_color: RendererInitialColor::CommittedConfig(initial_render_settings),
        },
    )
}

/// Пишет в лог, почему создаётся не тот рендер, которого ждал профиль.
fn log_selection_notice(family: RendererFamily, notice: &RendererSelectionNotice) {
    match notice {
        RendererSelectionNotice::RequestedFamilyNotBuilt { requested } => warn!(
            requested = requested.stable_id(),
            selected = family.stable_id(),
            "Requested renderer is not built into this version yet; starting the available renderer"
        ),
        RendererSelectionNotice::PreferredCandidateSkipped { skipped } => warn!(
            skipped = skipped.family.stable_id(),
            reason = ?skipped.probe,
            selected = family.stable_id(),
            "Preferred renderer skipped"
        ),
        RendererSelectionNotice::RetryingAfterUnfinishedStartup { family } => warn!(
            family = family.stable_id(),
            "The previous startup with this renderer did not finish; trying it again"
        ),
    }
}

#[cfg(test)]
mod tests {
    use fastiplayer_config::{AppConfig, VulkanPresentMode};
    use render_shell_api::{RendererFamily, RendererProbe, ShellPresentMode};

    use super::{
        RendererFamilyNotBuilt, platform_candidates, presentation_settings,
        probe_platform_candidates,
    };

    /// Перенастройка на месте получает от каталога тот же перевод `[render.vulkan]`, что
    /// фабрика при создании; семейства вне сборки — типизированный отказ, а не умолчания.
    #[test]
    fn presentation_settings_come_from_the_family_candidate() {
        let mut vulkan = AppConfig::default().render.vulkan;
        vulkan.present_mode = VulkanPresentMode::Mailbox;
        vulkan.max_frame_latency = 4;

        let settings = presentation_settings(RendererFamily::WgpuVulkan, &vulkan)
            .expect("wgpu candidate is built");
        assert_eq!(settings.present_mode, ShellPresentMode::Mailbox);
        assert_eq!(settings.max_frame_latency, 4);

        assert_eq!(
            presentation_settings(RendererFamily::Gles2, &vulkan),
            Err(RendererFamilyNotBuilt {
                family: RendererFamily::Gles2
            })
        );
    }

    /// Каталог Linux сессии 02: ровно один кандидат, wgpu/Vulkan, проба «годен».
    #[test]
    fn linux_catalog_has_only_the_wgpu_candidate_until_session_09() {
        let families: Vec<RendererFamily> = platform_candidates()
            .iter()
            .map(|candidate| candidate.family)
            .collect();
        assert_eq!(families, vec![RendererFamily::WgpuVulkan]);

        let probes = probe_platform_candidates();
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].family, RendererFamily::WgpuVulkan);
        assert_eq!(probes[0].probe, RendererProbe::Usable);
    }
}
