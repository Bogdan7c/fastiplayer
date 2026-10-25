//! Проба «годен ли wgpu/Vulkan на этой машине» без создания окна и устройства.
//!
//! Каталог рендеров приложения спрашивает пробу до создания рендера (сессия 09 плана
//! `user/gles2-renderer/`): на машине только с программным Vulkan (lavapipe/llvmpipe) или без
//! Vulkan вовсе wgpu-рендер не должен браться — играть через CPU-растеризатор дороже, чем
//! через GLES2 на аппаратном GPU. Проба только перечисляет адаптеры: ни `Device`, ни surface
//! не создаются, поэтому она дёшева и безопасна до появления окна.
//!
//! Решение вынесено в чистую функцию [`classify_selected_adapter`] над сводками адаптеров, чтобы
//! ветки «только CPU», «нет адаптеров» и «нет NV12» проверялись тестами без железа.

use render_shell_api::{RendererProbe, RendererUnusableReason, RequiredRendererFeature};

use crate::shell::gpu_context::{request_low_power_adapter, vulkan_instance};

/// Имя обязательной возможности адаптера, как её называет wgpu (`Features::TEXTURE_FORMAT_NV12`).
///
/// `required_wgpu_video_texture_features` требует NV12 всегда: без него декодер не может
/// отдать кадры рендеру, поэтому адаптер без NV12 негоден.
const REQUIRED_VIDEO_FEATURE_NAME: &str = "TEXTURE_FORMAT_NV12";

/// Сводка одного Vulkan-адаптера: ровно то, что нужно для решения «годен ли».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdapterSummary {
    /// Тип устройства по данным драйвера; `Cpu` — программный растеризатор.
    pub(crate) device_type: wgpu::DeviceType,

    /// Адаптер поддерживает обязательный для видео формат NV12.
    pub(crate) supports_required_video_format: bool,
}

/// Vulkan instance, созданный пробой; его можно отдать рендеру того же запуска.
///
/// Создание instance (загрузка Vulkan loader и слоёв, `vkCreateInstance`) — основная цена пробы
/// и ещё раз повторялась бы при создании рендера. Тип непрозрачный: приложение только носит
/// его от пробы к фабрике рендера ([`crate::Renderer::new_reusing_instance`]) и не знает про wgpu.
/// Instance не привязан к окну (`new_without_display_handle`), поэтому годится для любого окна.
pub struct ProbedVulkanInstance(wgpu::Instance);

impl ProbedVulkanInstance {
    /// Отдаёт instance рендеру; дальше им владеет рендер.
    pub(crate) fn into_instance(self) -> wgpu::Instance {
        self.0
    }
}

impl std::fmt::Debug for ProbedVulkanInstance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProbedVulkanInstance")
    }
}

/// Итог пробы вместе с instance, на котором она выполнена.
#[derive(Debug)]
pub struct VulkanProbeOutcome {
    /// Годен ли wgpu/Vulkan на этой машине.
    pub verdict: RendererProbe,

    /// Instance пробы: рендер того же запуска берёт его вместо создания второго.
    pub instance: ProbedVulkanInstance,
}

/// Проверяет, пригоден ли wgpu/Vulkan рендер на этой машине (без устройства и окна).
///
/// Instance пробы выбрасывается. Если рендер будет создан в этом же запуске, используйте
/// [`probe_hardware_vulkan_adapter_keeping_instance`].
#[must_use]
pub fn probe_hardware_vulkan_adapter() -> RendererProbe {
    probe_hardware_vulkan_adapter_keeping_instance().verdict
}

/// То же, что [`probe_hardware_vulkan_adapter`], но instance пробы возвращается вызывающему.
///
/// Оценивается ТОТ адаптер, который выберет создание рендера: тот же `request_low_power_adapter`
/// на том же instance (создание добавляет только совместимость с окном). Иначе машина «dGPU с
/// NV12 + iGPU без» получила бы `Usable` от пробы, а создание взяло бы iGPU и упало. Список
/// адаптеров нужен только для причины отказа (нет адаптеров / только программные).
#[must_use]
pub fn probe_hardware_vulkan_adapter_keeping_instance() -> VulkanProbeOutcome {
    let instance = vulkan_instance();
    // wgpu 30: перечисление — future; на Vulkan оно не ждёт ничего асинхронного.
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    let enumerated: Vec<AdapterSummary> = adapters.iter().map(summarize_adapter).collect();
    let selected = pollster::block_on(request_low_power_adapter(&instance, None))
        .ok()
        .map(|adapter| summarize_adapter(&adapter));
    VulkanProbeOutcome {
        verdict: classify_selected_adapter(selected, &enumerated),
        instance: ProbedVulkanInstance(instance),
    }
}

/// Сводка адаптера: тип устройства и поддержка NV12.
fn summarize_adapter(adapter: &wgpu::Adapter) -> AdapterSummary {
    AdapterSummary {
        device_type: adapter.get_info().device_type,
        supports_required_video_format: adapter
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_NV12),
    }
}

/// Чистое решение: годен ли адаптер, который выберет создание рендера.
///
/// - выбранный адаптер аппаратный и с NV12 → годен;
/// - выбранный аппаратный без NV12 → не хватает возможности (даже если другой адаптер с NV12
///   есть: создание возьмёт выбранный);
/// - выбранный программный (`DeviceType::Cpu`) → только программный адаптер;
/// - адаптер не выбран: список пуст или без аппаратных → причины по списку, иначе (аппаратные
///   есть, но запрос отказал) — нет аппаратного адаптера.
pub(crate) fn classify_selected_adapter(
    selected: Option<AdapterSummary>,
    enumerated: &[AdapterSummary],
) -> RendererProbe {
    let Some(selected) = selected else {
        let only_software = !enumerated.is_empty()
            && enumerated
                .iter()
                .all(|adapter| adapter.device_type == wgpu::DeviceType::Cpu);
        let reason = if only_software {
            RendererUnusableReason::SoftwareAdapterOnly
        } else {
            RendererUnusableReason::NoHardwareAdapter
        };
        return RendererProbe::Unusable { reason };
    };
    if selected.device_type == wgpu::DeviceType::Cpu {
        return RendererProbe::Unusable {
            reason: RendererUnusableReason::SoftwareAdapterOnly,
        };
    }
    if selected.supports_required_video_format {
        return RendererProbe::Usable;
    }
    RendererProbe::Unusable {
        reason: RendererUnusableReason::MissingRequiredFeature(RequiredRendererFeature::new(
            REQUIRED_VIDEO_FEATURE_NAME,
        )),
    }
}

#[cfg(test)]
mod tests {
    use render_shell_api::{RendererProbe, RendererUnusableReason, RequiredRendererFeature};

    use crate::shell::gpu_context::ensure_hardware_adapter;

    use super::{
        AdapterSummary, classify_selected_adapter, probe_hardware_vulkan_adapter,
        probe_hardware_vulkan_adapter_keeping_instance,
    };

    fn adapter(device_type: wgpu::DeviceType, nv12: bool) -> AdapterSummary {
        AdapterSummary {
            device_type,
            supports_required_video_format: nv12,
        }
    }

    fn unusable(reason: RendererUnusableReason) -> RendererProbe {
        RendererProbe::Unusable { reason }
    }

    fn missing_nv12() -> RendererProbe {
        unusable(RendererUnusableReason::MissingRequiredFeature(
            RequiredRendererFeature::new("TEXTURE_FORMAT_NV12"),
        ))
    }

    /// Таблица классификации: каждая ветка даёт свой исход, а не общий «негоден».
    #[test]
    fn classification_distinguishes_every_unusable_reason() {
        let cpu = adapter(wgpu::DeviceType::Cpu, true);
        let hardware_without_nv12 = adapter(wgpu::DeviceType::IntegratedGpu, false);
        let hardware_with_nv12 = adapter(wgpu::DeviceType::IntegratedGpu, true);
        assert_eq!(
            classify_selected_adapter(None, &[]),
            unusable(RendererUnusableReason::NoHardwareAdapter)
        );
        assert_eq!(
            classify_selected_adapter(None, &[cpu]),
            unusable(RendererUnusableReason::SoftwareAdapterOnly)
        );
        assert_eq!(
            classify_selected_adapter(None, &[hardware_with_nv12]),
            unusable(RendererUnusableReason::NoHardwareAdapter),
            "hardware exists but no adapter could be requested"
        );
        assert_eq!(
            classify_selected_adapter(Some(cpu), &[cpu]),
            unusable(RendererUnusableReason::SoftwareAdapterOnly)
        );
        assert_eq!(
            classify_selected_adapter(Some(hardware_without_nv12), &[hardware_without_nv12]),
            missing_nv12()
        );
        assert_eq!(
            classify_selected_adapter(Some(hardware_with_nv12), &[hardware_with_nv12]),
            RendererProbe::Usable
        );
    }

    /// Корень дефекта: оценивается выбранный создание адаптер, а не «любой из списка».
    /// dGPU с NV12 рядом с iGPU без NV12: выбран iGPU (LowPower) — проба обязана сказать
    /// «не хватает NV12», иначе создание упадёт после «годен». Обратный порядок — наоборот.
    #[test]
    fn the_selected_adapter_decides_not_any_adapter_in_the_list() {
        let discrete_with_nv12 = adapter(wgpu::DeviceType::DiscreteGpu, true);
        let integrated_without_nv12 = adapter(wgpu::DeviceType::IntegratedGpu, false);
        let both = [discrete_with_nv12, integrated_without_nv12];

        assert_eq!(
            classify_selected_adapter(Some(integrated_without_nv12), &both),
            missing_nv12()
        );
        assert_eq!(
            classify_selected_adapter(Some(discrete_with_nv12), &both),
            RendererProbe::Usable
        );
    }

    /// Настоящая проба на этой машине согласуется с независимым перечислением адаптеров:
    /// есть аппаратный адаптер с NV12 — `Usable`, иначе — одна из причин отказа (CI на
    /// llvmpipe получает `SoftwareAdapterOnly`, а не ложное «годен»).
    #[test]
    fn real_probe_matches_the_machine_adapter_list() {
        let instance = crate::shell::gpu_context::vulkan_instance();
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let has_usable_hardware = adapters.iter().any(|adapter| {
            adapter.get_info().device_type != wgpu::DeviceType::Cpu
                && adapter
                    .features()
                    .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
        });
        let has_software_only = !adapters.is_empty()
            && adapters
                .iter()
                .all(|adapter| adapter.get_info().device_type == wgpu::DeviceType::Cpu);

        let probe = probe_hardware_vulkan_adapter();

        if has_usable_hardware {
            assert_eq!(probe, RendererProbe::Usable);
        } else if has_software_only {
            assert_eq!(probe, unusable(RendererUnusableReason::SoftwareAdapterOnly));
        } else {
            assert_ne!(probe, RendererProbe::Usable);
        }
    }

    /// Создание рендера отвергает программный адаптер понятной ошибкой, аппаратный пропускает.
    #[test]
    fn renderer_creation_rejects_a_software_adapter_with_a_readable_error() {
        let error = ensure_hardware_adapter(wgpu::DeviceType::Cpu, "llvmpipe (LLVM 19)")
            .expect_err("software adapter must be rejected");
        assert!(error.to_string().contains("llvmpipe (LLVM 19)"));
        assert!(error.to_string().contains("hardware GPU is required"));
        for hardware in [
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::DiscreteGpu,
            wgpu::DeviceType::VirtualGpu,
        ] {
            assert!(ensure_hardware_adapter(hardware, "gpu").is_ok());
        }
    }

    /// Вариант пробы с сохранением instance даёт тот же вердикт, а возвращённый instance
    /// рабочий: на нём видны те же адаптеры, на которых принято решение (рендер возьмёт его
    /// вместо второго instance).
    #[test]
    fn probe_keeping_the_instance_agrees_and_hands_over_a_working_instance() {
        let outcome = probe_hardware_vulkan_adapter_keeping_instance();

        assert_eq!(outcome.verdict, probe_hardware_vulkan_adapter());
        let instance = outcome.instance.into_instance();
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        assert_eq!(
            adapters.is_empty(),
            outcome.verdict == unusable(RendererUnusableReason::NoHardwareAdapter),
            "the handed-over instance must see the adapters the verdict was based on"
        );
    }
}
