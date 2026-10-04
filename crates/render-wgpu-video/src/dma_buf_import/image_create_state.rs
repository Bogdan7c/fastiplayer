//! Параметры создания `VkImage` для импортированного DMA-BUF, которые обязаны
//! совпадать одновременно со спецификацией Vulkan и с состоянием tracker-а wgpu.
//!
//! Модуль чистый: он не вызывает Vulkan и не владеет ресурсами, поэтому его
//! решения проверяются hermetic-тестами без GPU. Оба пути импорта
//! (multi-planar и separate-layer) берут начальное состояние только отсюда,
//! чтобы их семантика не разошлась.

use ash::vk;

use super::{DmaBufFrameFormat, plane_view_contract_for_imported_format};

/// Согласованная пара начальных состояний импортированного image.
///
/// Первое поле говорит Vulkan, в каком layout создаётся `VkImage`. Второе
/// говорит wgpu-core, из какого состояния строить первый барьер. Они описывают
/// одно и то же «содержимое не определено для Vulkan», поэтому tracker wgpu
/// никогда не считает image находящимся в layout-е, которого нет на самом деле.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImportedImageStartState {
    /// `VkImageCreateInfo::initialLayout`.
    pub(super) vulkan_initial_layout: vk::ImageLayout,

    /// `initial_state` для `wgpu::Device::create_texture_from_hal`.
    pub(super) wgpu_initial_state: wgpu::TextureUses,
}

/// Возвращает начальное состояние для любого DMA-BUF image (LINEAR и DRM modifier).
///
/// Почему именно `UNDEFINED` + `UNINITIALIZED`:
/// - `VUID-VkImageCreateInfo-pNext-01443`: если в `pNext` есть
///   `VkExternalMemoryImageCreateInfo` с ненулевым `handleTypes`, `initialLayout`
///   обязан быть `UNDEFINED`. Исключений по tiling нет, поэтому выбор не зависит
///   от modifier-а. Прежний `PREINITIALIZED` нарушал это правило, а для
///   DRM modifier tiling ещё и `VUID-VkImageCreateInfo-initialLayout-12478`.
/// - Спецификация (External Resource Sharing) прямо называет `UNDEFINED` при
///   создании image поверх чужой памяти «placeholder»: реальное содержимое
///   записал внешний producer (VA-API), Vulkan о нём просто ничего не знает.
/// - wgpu-hal 30 отображает `TextureUses::UNINITIALIZED` в `VK_IMAGE_LAYOUT_UNDEFINED`
///   (`derive_image_layout`), поэтому первый барьер wgpu
///   `UNDEFINED -> SHADER_READ_ONLY_OPTIMAL` начинается ровно с того layout-а,
///   в котором image создан. Так же делает собственный импорт wgpu-hal
///   (`texture_from_dmabuf_fd`).
///
/// Известное ограничение (решение владельца, сессия 03 обновления egui/wgpu):
/// переход из `UNDEFINED` формально разрешает драйверу не сохранять содержимое.
/// Для несжатых modifiers (наш AMD GFX11 `64K_R_X` без DCC) драйверу нечего
/// выбрасывать. Строгая гарантия для сжатых modifiers требует acquire-барьера из
/// `VK_QUEUE_FAMILY_FOREIGN_EXT`, который wgpu 30 сам не записывает; это бэклог.
pub(super) const fn imported_dma_buf_start_state() -> ImportedImageStartState {
    ImportedImageStartState {
        vulkan_initial_layout: vk::ImageLayout::UNDEFINED,
        wgpu_initial_state: wgpu::TextureUses::UNINITIALIZED,
    }
}

/// Форматы, в которых создаются view multi-planar image: сам формат image и
/// форматы двух plane view (Y и UV).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MultiPlanarViewFormats {
    /// Порядок: формат image, формат Y plane view, формат UV plane view.
    formats: [vk::Format; 3],
}

impl MultiPlanarViewFormats {
    /// Слайс для `VkImageFormatListCreateInfo::pViewFormats`.
    pub(super) const fn as_slice(&self) -> &[vk::Format] {
        &self.formats
    }
}

/// Строит список форматов для `VkImageFormatListCreateInfo` multi-planar image.
///
/// Multi-planar image создаётся с `MUTABLE_FORMAT`, потому что plane view
/// (`R8`/`RG8` для NV12, `R16`/`RG16` для P010) имеют формат, отличный от формата
/// image. `VUID-VkImageCreateInfo-tiling-02353` требует для DRM modifier tiling с
/// `MUTABLE_FORMAT` явный список таких форматов. Список строится из того же
/// plane view contract-а, по которому потом создаются view, поэтому они не
/// разойдутся.
pub(super) fn multi_planar_view_formats(
    frame_format: DmaBufFrameFormat,
) -> anyhow::Result<MultiPlanarViewFormats> {
    let view_contract = plane_view_contract_for_imported_format(frame_format);
    let y_plane_format = plane_view_vulkan_format(view_contract.y_plane.format)?;
    let uv_plane_format = plane_view_vulkan_format(view_contract.uv_plane.format)?;

    Ok(MultiPlanarViewFormats {
        formats: [
            frame_format.vulkan_texture_format(),
            y_plane_format,
            uv_plane_format,
        ],
    })
}

/// Переводит wgpu формат plane view в Vulkan формат, который выберет wgpu-hal
/// (`conv::map_texture_format`) при создании этого view.
fn plane_view_vulkan_format(plane_format: wgpu::TextureFormat) -> anyhow::Result<vk::Format> {
    match plane_format {
        wgpu::TextureFormat::R8Unorm => Ok(vk::Format::R8_UNORM),
        wgpu::TextureFormat::Rg8Unorm => Ok(vk::Format::R8G8_UNORM),
        wgpu::TextureFormat::R16Unorm => Ok(vk::Format::R16_UNORM),
        wgpu::TextureFormat::Rg16Unorm => Ok(vk::Format::R16G16_UNORM),
        unsupported => anyhow::bail!(
            "DMA-BUF plane view format {unsupported:?} has no Vulkan format mapping for VkImageFormatListCreateInfo"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiling-и, которые реально встречаются в импорте: modifier == 0 и любой другой.
    const IMPORT_TILINGS: [vk::ImageTiling; 2] = [
        vk::ImageTiling::LINEAR,
        vk::ImageTiling::from_raw(super::super::VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
    ];

    /// Правила спецификации Vulkan для `initialLayout` image с external memory.
    ///
    /// Кодирует `VUID-VkImageCreateInfo-initialLayout-00993` (допустимые значения),
    /// `VUID-VkImageCreateInfo-pNext-01443` (external memory → только `UNDEFINED`)
    /// и `VUID-VkImageCreateInfo-initialLayout-12478` (`PREINITIALIZED` только LINEAR).
    fn spec_allows_external_import_initial_layout(
        layout: vk::ImageLayout,
        tiling: vk::ImageTiling,
    ) -> bool {
        let generally_allowed = matches!(
            layout,
            vk::ImageLayout::UNDEFINED | vk::ImageLayout::PREINITIALIZED
        );
        let external_memory_allowed = layout == vk::ImageLayout::UNDEFINED;
        let tiling_allowed =
            layout != vk::ImageLayout::PREINITIALIZED || tiling == vk::ImageTiling::LINEAR;
        generally_allowed && external_memory_allowed && tiling_allowed
    }

    /// Прежнее значение `PREINITIALIZED` действительно нарушает правила — иначе
    /// следующий тест проверял бы пустой helper.
    #[test]
    fn previous_preinitialized_layout_violates_spec_for_every_import_tiling() {
        for tiling in IMPORT_TILINGS {
            assert!(
                !spec_allows_external_import_initial_layout(
                    vk::ImageLayout::PREINITIALIZED,
                    tiling
                ),
                "PREINITIALIZED must be rejected for external memory with {tiling:?}"
            );
        }
    }

    /// Выбранный layout допустим и для LINEAR, и для DRM modifier tiling.
    #[test]
    fn start_layout_is_spec_valid_for_linear_and_drm_modifier_imports() {
        let start_state = imported_dma_buf_start_state();

        for tiling in IMPORT_TILINGS {
            assert!(
                spec_allows_external_import_initial_layout(
                    start_state.vulkan_initial_layout,
                    tiling
                ),
                "{:?} must be valid for external DMA-BUF import with {tiling:?}",
                start_state.vulkan_initial_layout
            );
        }
    }

    /// Состояние tracker-а wgpu описывает тот же layout, в котором создан image:
    /// wgpu-hal 30 переводит `UNINITIALIZED` именно в `UNDEFINED` для первого барьера.
    #[test]
    fn wgpu_tracker_state_matches_vulkan_initial_layout() {
        let start_state = imported_dma_buf_start_state();

        assert_eq!(
            start_state.vulkan_initial_layout,
            vk::ImageLayout::UNDEFINED
        );
        assert_eq!(
            start_state.wgpu_initial_state,
            wgpu::TextureUses::UNINITIALIZED
        );
    }

    /// NV12: формат image + R8 (Y) + RG8 (UV), ровно те, что использует wgpu-hal для view.
    #[test]
    fn nv12_view_format_list_contains_image_and_both_plane_formats() {
        let view_formats =
            multi_planar_view_formats(DmaBufFrameFormat::Nv12).expect("NV12 formats are mapped");

        assert_eq!(
            view_formats.as_slice(),
            &[
                vk::Format::G8_B8R8_2PLANE_420_UNORM,
                vk::Format::R8_UNORM,
                vk::Format::R8G8_UNORM,
            ]
        );
    }

    /// P010: формат image + R16 (Y) + RG16 (UV).
    #[test]
    fn p010_view_format_list_contains_image_and_both_plane_formats() {
        let view_formats =
            multi_planar_view_formats(DmaBufFrameFormat::P010).expect("P010 formats are mapped");

        assert_eq!(
            view_formats.as_slice(),
            &[
                vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
                vk::Format::R16_UNORM,
                vk::Format::R16G16_UNORM,
            ]
        );
    }

    /// Неизвестный формат view не превращается молча в `UNDEFINED`, а даёт ошибку.
    #[test]
    fn unmapped_plane_view_format_is_reported_as_error() {
        let error = plane_view_vulkan_format(wgpu::TextureFormat::Rgba8Unorm)
            .expect_err("Rgba8Unorm is not a DMA-BUF plane view format");

        assert!(
            error.to_string().contains("Rgba8Unorm"),
            "error must name the unmapped format: {error}"
        );
    }
}
