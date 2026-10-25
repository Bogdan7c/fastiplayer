//! Цель импорта zero-copy поверхностей: в какой API рендер принимает DMA-BUF декодера.
//!
//! Декодер VA-API один и тот же, но рендеры принимают его кадры по-разному: wgpu — через
//! Vulkan (`VK_EXT_external_memory_dma_buf`), GLES2 — через EGL (`EGL_EXT_image_dma_buf_import`).
//! Тип нейтральный: он не знает ни про `render-shell-api`, ни про конкретные рендеры (иначе
//! декодер зависел бы от рендера). Приложение само переводит семейство рендера в цель.
//!
//! От цели зависит только имя контракта в логе (`backend_contract=...`): по нему
//! `docs/benchmarks/tools/local/evidence.py` доказывает, на каком рендере шёл аппаратный путь.
//! Остальные обещания контракта (sample-only, ожидание GPU, проверка идентичности) для обоих
//! рендеров одинаковы: оба кэшируют импорт по идентичности буфера и возвращают поверхность
//! декодеру только после завершения GPU-работы.

use crate::zero_copy_surface_pool::ZeroCopyImportReuseContract;

/// В какой графический API рендер импортирует DMA-BUF кадры декодера.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ZeroCopyImportTarget {
    /// Vulkan (рендер wgpu). Прежнее поведение, значение по умолчанию.
    #[default]
    VulkanDmaBuf,

    /// EGL (рендер OpenGL ES 2.0).
    EglDmaBuf,
}

impl ZeroCopyImportReuseContract {
    /// Контракт VA-API zero-copy пути для выбранной цели импорта.
    #[must_use]
    pub const fn for_import_target(target: ZeroCopyImportTarget) -> Self {
        match target {
            ZeroCopyImportTarget::VulkanDmaBuf => Self::vaapi_vulkan_dma_buf(),
            ZeroCopyImportTarget::EglDmaBuf => Self::vaapi_egl_dma_buf(),
        }
    }

    /// Контракт VA-API → EGL DMA-BUF (GLES2): отличается от Vulkan-контракта только именем.
    ///
    /// Имя «VA-API EGL DMA-BUF» — строка, которую ищет `evidence.py` как доказательство
    /// аппаратного пути на GLES2; менять её можно только вместе с этим инструментом.
    #[must_use]
    pub const fn vaapi_egl_dma_buf() -> Self {
        Self {
            backend_name: "VA-API EGL DMA-BUF",
            ..Self::vaapi_vulkan_dma_buf()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ZeroCopyImportTarget;
    use crate::resource_pool::FrameResourcePool;
    use crate::zero_copy_surface_pool::ZeroCopyImportReuseContract;

    /// Имена контрактов — строки, на которые опирается evidence-инструмент: байт-в-байт.
    #[test]
    fn contract_names_match_the_evidence_tool_strings() {
        assert_eq!(
            ZeroCopyImportReuseContract::for_import_target(ZeroCopyImportTarget::VulkanDmaBuf)
                .backend_name,
            "VA-API Vulkan DMA-BUF"
        );
        assert_eq!(
            ZeroCopyImportReuseContract::for_import_target(ZeroCopyImportTarget::EglDmaBuf)
                .backend_name,
            "VA-API EGL DMA-BUF"
        );
    }

    /// EGL-контракт отличается только именем: гарантии reuse не ослаблены.
    #[test]
    fn egl_contract_keeps_every_reuse_guarantee_of_the_vulkan_one() {
        let egl = ZeroCopyImportReuseContract::vaapi_egl_dma_buf();
        let vulkan = ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf();

        assert_eq!(
            ZeroCopyImportReuseContract {
                backend_name: vulkan.backend_name,
                ..egl
            },
            vulkan
        );
    }

    /// Цель доезжает до пула кадров, который декодер пишет в лог: Vulkan по умолчанию.
    #[test]
    fn frame_pool_carries_the_requested_import_target() {
        assert_eq!(
            FrameResourcePool::new().reuse_contract().backend_name,
            "VA-API Vulkan DMA-BUF"
        );
        assert_eq!(
            FrameResourcePool::new_with_import_target(4, ZeroCopyImportTarget::EglDmaBuf)
                .reuse_contract()
                .backend_name,
            "VA-API EGL DMA-BUF"
        );
        assert_eq!(
            FrameResourcePool::new_with_import_target(4, ZeroCopyImportTarget::VulkanDmaBuf)
                .reuse_contract()
                .backend_name,
            "VA-API Vulkan DMA-BUF"
        );
    }
}
