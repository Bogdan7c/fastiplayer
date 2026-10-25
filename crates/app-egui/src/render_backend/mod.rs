//! Каталог рендеров платформы (решение владельца 3 плана `user/gles2-renderer/`).
//!
//! Приложение знает рендер только как `Box<dyn RenderShell>`. Какие рендеры есть на
//! платформе, в каком порядке их предпочитать и как создать конкретный — знает только этот
//! модуль. Это единственное место `app-egui`, где упоминаются конкретные рендеры: добавить
//! DX10 — значит дописать кандидата в [`platform_candidates`] и его фабрику, не трогая
//! остальное приложение.
//!
//! Создание рендера идёт только отсюда: при старте ([`create_startup_renderer`]: ленивые
//! пробы, [`select_renderer`](selection::select_renderer), фолбэк при ошибке создания) и при
//! пересоздании внутри семейства ([`create_renderer`]).

mod gles2;
mod probe_leftovers;
mod report;
mod selection;
mod startup;
mod wgpu_vulkan;

use std::sync::Arc;

use fastiplayer_config::{AppConfig, VulkanConfig};
use render_shell_api::{
    RenderShell, RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe,
    SurfacePresentSettings,
};
use winit::window::Window;

use crate::settings_runtime::InitialRenderSettings;

pub(crate) use probe_leftovers::ProbeLeftovers;
pub(crate) use report::RendererStartupReport;
pub(crate) use selection::RendererStartupMarker;
// Нужны только тестам соседних модулей, которые строят отчёты выбора руками.
#[cfg(test)]
pub(crate) use selection::{CandidateProbe, SelectionReason};
pub(crate) use startup::{StartedRenderer, UnfinishedStartupTracker};
use startup::{StartupCandidates, start_renderer};

/// Что нужно фабрике кандидата для создания рендера.
pub(crate) struct RendererCreateRequest<'config> {
    /// Окно, на котором рендер создаёт свою цель показа (и которому шлёт `pre_present_notify`).
    pub(crate) window: Arc<Window>,

    /// Настройки показа Vulkan/wgpu (`[render.vulkan]`): present mode и задержка swapchain.
    pub(crate) vulkan: &'config VulkanConfig,

    /// Какие настройки цвета у рендера сразу после создания.
    pub(crate) initial_color: RendererInitialColor,

    /// Что оставила проба этого запуска (пусто при пересоздании рендера).
    pub(crate) probe_leftovers: ProbeLeftovers,
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
    ///
    /// Проба кладёт в остатки то, что дорого создавать дважды (instance Vulkan): фабрика
    /// того же запуска их заберёт.
    probe: fn(&mut ProbeLeftovers) -> RendererProbe,

    /// Фабрика рендера.
    create: fn(RendererCreateRequest<'_>) -> Result<Box<dyn RenderShell>, RendererCreateError>,

    /// Перевод committed настроек показа в нейтральные (тот же, что у фабрики при создании):
    /// им пользуется перенастройка показа на месте (сессия 02a).
    presentation_settings: fn(&VulkanConfig) -> SurfacePresentSettings,
}

/// Кандидаты платформы в порядке предпочтения.
fn platform_candidates() -> &'static [RendererCandidate] {
    const LINUX_CANDIDATES: &[RendererCandidate] = &[
        RendererCandidate {
            family: RendererFamily::WgpuVulkan,
            probe: wgpu_vulkan::probe,
            create: wgpu_vulkan::create,
            presentation_settings: wgpu_vulkan::surface_present_settings,
        },
        RendererCandidate {
            family: RendererFamily::Gles2,
            probe: gles2::probe,
            create: gles2::create,
            presentation_settings: gles2::surface_present_settings,
        },
    ];
    LINUX_CANDIDATES
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

/// Кандидаты платформы в роли источника для старта: пробы и фабрики с данными запуска.
struct PlatformStartup<'config> {
    /// Окно, на котором создаётся рендер.
    window: Arc<Window>,

    /// Настройки показа Vulkan/wgpu.
    vulkan: &'config VulkanConfig,

    /// Стартовые настройки цвета (клонируются на каждую попытку создания).
    initial_render_settings: InitialRenderSettings,

    /// Остатки проб; фабрика рендера забирает их при создании.
    probe_leftovers: ProbeLeftovers,
}

impl StartupCandidates for PlatformStartup<'_> {
    type Renderer = Box<dyn RenderShell>;

    fn catalog_order(&self) -> Vec<RendererFamily> {
        platform_candidates()
            .iter()
            .map(|candidate| candidate.family)
            .collect()
    }

    fn probe(&mut self, family: RendererFamily) -> RendererProbe {
        match platform_candidates()
            .iter()
            .find(|candidate| candidate.family == family)
        {
            Some(candidate) => (candidate.probe)(&mut self.probe_leftovers),
            // Семейство берётся из `catalog_order`, поэтому сюда не попадает; на всякий случай
            // отвечаем честным «нет», а не паникой.
            None => RendererProbe::Unusable {
                reason: render_shell_api::RendererUnusableReason::NoHardwareAdapter,
            },
        }
    }

    fn create(
        &mut self,
        family: RendererFamily,
    ) -> Result<Box<dyn RenderShell>, RendererCreateError> {
        create_renderer(
            family,
            RendererCreateRequest {
                window: self.window.clone(),
                vulkan: self.vulkan,
                initial_color: RendererInitialColor::CommittedConfig(
                    self.initial_render_settings.clone(),
                ),
                // Остатки проб достаются первой же попытке создания; фолбэк-попытки без них.
                probe_leftovers: std::mem::take(&mut self.probe_leftovers),
            },
        )
    }
}

/// Выбирает семейство по профилю из config-а и создаёт рендер для старта приложения.
///
/// Пробы ленивые, при ошибке создания идёт фолбэк на следующего годного кандидата (решения
/// 35, 39, 40 плана gles2-renderer). Метку незавершённого старта читает и пишет `tracker`
/// (владелец — `app_shell`). Возвращает рендер и нейтральный отчёт выбора.
pub(crate) fn create_startup_renderer(
    config: &AppConfig,
    window: Arc<Window>,
    initial_render_settings: InitialRenderSettings,
    tracker: &mut dyn UnfinishedStartupTracker,
) -> Result<StartedRenderer<Box<dyn RenderShell>>, RendererCreateError> {
    let mut startup = PlatformStartup {
        window,
        vulkan: &config.render.vulkan,
        initial_render_settings,
        probe_leftovers: ProbeLeftovers::default(),
    };
    start_renderer(config.render.profile, &mut startup, tracker)
}

#[cfg(test)]
mod tests {
    use fastiplayer_config::{AppConfig, VulkanPresentMode};
    use render_shell_api::{RendererFamily, ShellPresentMode};

    use super::{platform_candidates, presentation_settings};

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

        // GLES2 не зависит от `[render.vulkan]`: vsync при любых значениях секции.
        let gles2 = presentation_settings(RendererFamily::Gles2, &vulkan)
            .expect("gles2 candidate is built");
        assert_eq!(gles2.present_mode, ShellPresentMode::Fifo);
    }

    /// Каталог Linux: wgpu/Vulkan первым, GLES2 вторым (порядок = предпочтение).
    #[test]
    fn linux_catalog_prefers_wgpu_then_gles2() {
        let families: Vec<RendererFamily> = platform_candidates()
            .iter()
            .map(|candidate| candidate.family)
            .collect();
        assert_eq!(
            families,
            vec![RendererFamily::WgpuVulkan, RendererFamily::Gles2]
        );
    }
}
