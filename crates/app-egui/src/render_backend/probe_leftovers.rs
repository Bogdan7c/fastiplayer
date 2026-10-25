//! То, что проба кандидата оставила для создания рендера в том же запуске.
//!
//! Проба wgpu/Vulkan создаёт Vulkan instance (основная цена пробы: loader, слои,
//! `vkCreateInstance`; на 780M около 20 мс холодной), и рендер сразу после этого создал бы ещё
//! один. Вместо глобального кэша instance едет по явной цепочке: проба кладёт его сюда, фабрика
//! кандидата забирает. Каталог сам не знает, что внутри: тип нейтрален для остальных семейств.

use render_wgpu_shell::ProbedVulkanInstance;

/// Остатки пробы; пусто по умолчанию (пересоздание рендера и семейства без пробы).
#[derive(Debug, Default)]
pub(crate) struct ProbeLeftovers {
    /// Vulkan instance пробы wgpu/Vulkan; `None` — пробы не было или instance уже забрали.
    wgpu_vulkan_instance: Option<ProbedVulkanInstance>,
}

impl ProbeLeftovers {
    /// Проба wgpu/Vulkan оставляет свой instance (прежний, если был, выбрасывается).
    pub(super) fn keep_wgpu_vulkan_instance(&mut self, instance: ProbedVulkanInstance) {
        self.wgpu_vulkan_instance = Some(instance);
    }

    /// Фабрика wgpu/Vulkan забирает instance пробы; второй раз он не выдаётся.
    pub(super) fn take_wgpu_vulkan_instance(&mut self) -> Option<ProbedVulkanInstance> {
        self.wgpu_vulkan_instance.take()
    }

    /// Есть ли у остатков instance wgpu/Vulkan (для проверок порядка проба → создание).
    #[cfg(test)]
    pub(super) fn holds_wgpu_vulkan_instance(&self) -> bool {
        self.wgpu_vulkan_instance.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use render_wgpu_shell::probe_hardware_vulkan_adapter_keeping_instance;

    /// Instance пробы выдаётся ровно один раз; пустые остатки (пересоздание) ничего не выдают.
    #[test]
    fn the_probed_instance_is_handed_out_exactly_once() {
        let mut empty = ProbeLeftovers::default();
        assert!(empty.take_wgpu_vulkan_instance().is_none());

        let mut leftovers = ProbeLeftovers::default();
        leftovers
            .keep_wgpu_vulkan_instance(probe_hardware_vulkan_adapter_keeping_instance().instance);
        assert!(leftovers.holds_wgpu_vulkan_instance());

        assert!(leftovers.take_wgpu_vulkan_instance().is_some());
        assert!(!leftovers.holds_wgpu_vulkan_instance());
        assert!(leftovers.take_wgpu_vulkan_instance().is_none());
    }
}
