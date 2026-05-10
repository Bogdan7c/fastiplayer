/// Zero-copy импорт DMA-BUF fd в wgpu texture через Vulkan external memory.
///
/// Использует VK_EXT_external_memory_dma_buf + VK_KHR_external_memory_fd
/// для импорта dma-buf напрямую в VkImage без CPU readback.
///
/// Почему это нужно:
/// GenericDmaVideoFrame::map() делает blocking poll(PollTimeout::NONE) на DMA-BUF fence,
/// что занимает 700-1000 мс на Intel i965 для 4K NV12. Это делает декод ~1 FPS.
/// DMA-BUF fd уже содержит decoded frame в GPU-visible memory (shared system memory на Intel).
/// Импорт fd в Vulkan image позволяет использовать decoded frame zero-copy.
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use ash::vk;
use cros_codecs::decoder::{DecodedDmaBufExportLayout, DecodedDmaBufImage, DecodedDmaBufLayer};
use cros_codecs::libva::ExternalBufferDescriptor;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;

/// Значение VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT из Vulkan headers.
/// ash 0.38 не экспортирует эту константу напрямую.
const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i32 = 1000158000;

/// DRM fourcc для NV12 (`'N' 'V' '1' '2'` в little-endian представлении).
const DRM_FORMAT_NV12: u32 = 0x3231_564e;

/// DRM fourcc для P010 (`'P' '0' '1' '0'` в little-endian представлении).
const DRM_FORMAT_P010: u32 = 0x3031_3050;

/// DRM fourcc для отдельной 8-bit luma plane (`R8`).
const DRM_FORMAT_R8: u32 = 0x2020_3852;

/// DRM fourcc для отдельной 8-bit interleaved chroma plane (`GR88`).
const DRM_FORMAT_GR88: u32 = 0x3838_5247;

/// DRM fourcc для отдельной 16-bit luma plane (`R16`).
const DRM_FORMAT_R16: u32 = 0x2036_3152;

/// DRM fourcc для отдельной 16-bit interleaved chroma plane (`GR32` / `GR1616`).
const DRM_FORMAT_GR1616: u32 = 0x3233_5247;

/// Значение DRM_FORMAT_MOD_LINEAR = 0 (linear, untiled).
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Формат decoded DMA-BUF descriptor-а, который можно импортировать в wgpu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmaBufFrameFormat {
    /// 8-bit 4:2:0 NV12: Y plane + interleaved UV plane.
    Nv12,

    /// 10-bit 4:2:0 P010: Y plane + interleaved UV plane в 16-bit контейнере.
    P010,
}

impl DmaBufFrameFormat {
    /// Определяет формат по DRM fourcc из exported VA descriptor-а.
    pub(crate) fn from_fourcc(image_fourcc: u32, layer_fourcc: u32) -> anyhow::Result<Self> {
        match (image_fourcc, layer_fourcc) {
            (DRM_FORMAT_NV12, DRM_FORMAT_NV12) => Ok(Self::Nv12),
            (DRM_FORMAT_P010, DRM_FORMAT_P010) => Ok(Self::P010),
            _ => anyhow::bail!(
                "unsupported exported VA surface format: image_fourcc={:#x}, layer_fourcc={:#x}",
                image_fourcc,
                layer_fourcc
            ),
        }
    }

    /// Определяет формат по separate-layer DRM descriptor-у VA-API.
    pub(crate) fn from_separate_layers(
        image_fourcc: u32,
        layers: &[DecodedDmaBufLayer],
    ) -> anyhow::Result<Self> {
        let y_layer = layers
            .first()
            .context("separate-layer DMA-BUF image has no luma layer")?;
        let uv_layer = layers
            .get(1)
            .context("separate-layer DMA-BUF image has no chroma layer")?;

        if y_layer.num_planes != 1 || uv_layer.num_planes != 1 {
            anyhow::bail!(
                "separate-layer DMA-BUF descriptor must use one plane per layer: y_planes={}, uv_planes={}",
                y_layer.num_planes,
                uv_layer.num_planes
            );
        }

        match (image_fourcc, y_layer.drm_format, uv_layer.drm_format) {
            (DRM_FORMAT_NV12, DRM_FORMAT_R8, DRM_FORMAT_GR88) => Ok(Self::Nv12),
            (DRM_FORMAT_P010, DRM_FORMAT_R16, DRM_FORMAT_GR1616) => Ok(Self::P010),
            _ => anyhow::bail!(
                "unsupported separate-layer VA surface format: image_fourcc={:#x}, y_fourcc={:#x}, uv_fourcc={:#x}",
                image_fourcc,
                y_layer.drm_format,
                uv_layer.drm_format
            ),
        }
    }

    /// Возвращает wgpu multi-planar texture format для whole-image import.
    pub(crate) const fn wgpu_texture_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Nv12 => wgpu::TextureFormat::NV12,
            Self::P010 => wgpu::TextureFormat::P010,
        }
    }

    /// Возвращает Vulkan format, совместимый с DRM descriptor-ом VA-API.
    const fn vulkan_texture_format(self) -> vk::Format {
        match self {
            Self::Nv12 => vk::Format::G8_B8R8_2PLANE_420_UNORM,
            Self::P010 => vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
        }
    }

    /// Возвращает feature, который wgpu требует для создания такого texture format.
    const fn required_wgpu_feature(self) -> wgpu::Features {
        match self {
            Self::Nv12 => wgpu::Features::TEXTURE_FORMAT_NV12,
            Self::P010 => wgpu::Features::TEXTURE_FORMAT_P010,
        }
    }

    /// Возвращает имя required feature для понятной ошибки без Debug шума.
    const fn required_wgpu_feature_name(self) -> &'static str {
        match self {
            Self::Nv12 => "TEXTURE_FORMAT_NV12",
            Self::P010 => "TEXTURE_FORMAT_P010",
        }
    }

    /// Возвращает короткий форматный label для diagnostics.
    pub(crate) const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
        }
    }

    /// Возвращает label для imported wgpu texture.
    const fn texture_label(self) -> &'static str {
        match self {
            Self::Nv12 => "dma-buf-imported-nv12",
            Self::P010 => "dma-buf-imported-p010",
        }
    }

    /// Возвращает label для luma view.
    const fn y_view_label(self) -> &'static str {
        match self {
            Self::Nv12 => "dma-buf-imported-nv12-y",
            Self::P010 => "dma-buf-imported-p010-y",
        }
    }

    /// Возвращает label для chroma view.
    const fn uv_view_label(self) -> &'static str {
        match self {
            Self::Nv12 => "dma-buf-imported-nv12-uv",
            Self::P010 => "dma-buf-imported-p010-uv",
        }
    }

    /// Проверяет, что wgpu device был создан с feature для нужного texture format.
    fn ensure_device_feature(self, device_features: wgpu::Features) -> anyhow::Result<()> {
        let required_feature = self.required_wgpu_feature();
        if device_features.contains(required_feature) {
            return Ok(());
        }

        anyhow::bail!(
            "wgpu device was created without {} support required for {} DMA-BUF import",
            self.required_wgpu_feature_name(),
            self.diagnostic_label()
        );
    }

    /// Проверяет feature gate для baseline path, где VA отдаёт P010 как R16/GR32 layers.
    fn ensure_separate_layer_device_features(
        self,
        device_features: wgpu::Features,
    ) -> anyhow::Result<()> {
        if self != Self::P010 || device_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
        {
            return Ok(());
        }

        anyhow::bail!(
            "wgpu device was created without TEXTURE_FORMAT_16BIT_NORM support required for separate-layer P010 DMA-BUF import"
        );
    }
}

/// Описание одного plane view, создаваемого поверх multi-planar texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaBufPlaneViewContract {
    /// Формат plane view, который видит shader binding.
    pub format: wgpu::TextureFormat,

    /// Plane aspect внутри multi-planar texture.
    pub aspect: wgpu::TextureAspect,
}

/// Полный view contract для импортированного NV12/P010 texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaBufViewContract {
    /// Whole-image texture format.
    pub texture_format: wgpu::TextureFormat,

    /// Luma/Y plane view.
    pub y_plane: DmaBufPlaneViewContract,

    /// Interleaved chroma/UV plane view.
    pub uv_plane: DmaBufPlaneViewContract,
}

/// Возвращает plane view contract для imported multi-planar texture.
pub(crate) const fn plane_view_contract_for_imported_format(
    frame_format: DmaBufFrameFormat,
) -> DmaBufViewContract {
    match frame_format {
        DmaBufFrameFormat::Nv12 => DmaBufViewContract {
            texture_format: wgpu::TextureFormat::NV12,
            y_plane: DmaBufPlaneViewContract {
                format: wgpu::TextureFormat::R8Unorm,
                aspect: wgpu::TextureAspect::Plane0,
            },
            uv_plane: DmaBufPlaneViewContract {
                format: wgpu::TextureFormat::Rg8Unorm,
                aspect: wgpu::TextureAspect::Plane1,
            },
        },
        DmaBufFrameFormat::P010 => DmaBufViewContract {
            texture_format: wgpu::TextureFormat::P010,
            y_plane: DmaBufPlaneViewContract {
                format: wgpu::TextureFormat::R16Unorm,
                aspect: wgpu::TextureAspect::Plane0,
            },
            uv_plane: DmaBufPlaneViewContract {
                format: wgpu::TextureFormat::Rg16Unorm,
                aspect: wgpu::TextureAspect::Plane1,
            },
        },
    }
}

/// Дублирует DMA-BUF fd перед передачей во Vulkan import.
///
/// Vulkan получает владение fd только после успешного `vkAllocateMemory`.
/// Поэтому fd закрывается нашим кодом только на ошибке аллокации.
fn duplicate_fd_for_vulkan_import(
    source_fd: RawFd,
    import_context: &'static str,
) -> anyhow::Result<RawFd> {
    nix::unistd::dup(source_fd)
        .with_context(|| format!("{import_context}: dup dma-buf fd for Vulkan import failed"))
}

/// Закрывает fd, который Vulkan не принял из-за ошибки import.
fn close_unimported_fd(vulkan_fd: RawFd, import_context: &'static str) {
    if let Err(error) = nix::unistd::close(vulkan_fd) {
        tracing::warn!(
            error = %error,
            fd = vulkan_fd,
            import_context,
            "Failed to close DMA-BUF fd during import cleanup"
        );
    }
}

/// Аллоцирует `VkDeviceMemory`, импортируя DMA-BUF fd.
///
/// Важное правило Vulkan: при успешном импорте fd становится собственностью
/// Vulkan implementation, поэтому вызывающий код больше не закрывает этот fd.
fn allocate_dma_buf_memory_for_image(
    raw_device: &ash::Device,
    image: vk::Image,
    mem_requirements: vk::MemoryRequirements,
    memory_type_index: u32,
    source_fd: RawFd,
    import_context: &'static str,
) -> anyhow::Result<vk::DeviceMemory> {
    let vulkan_fd = duplicate_fd_for_vulkan_import(source_fd, import_context)?;
    let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let mut import_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(vulkan_fd);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(mem_requirements.size)
        .memory_type_index(memory_type_index)
        .push_next(&mut dedicated_info)
        .push_next(&mut import_info);

    match unsafe { raw_device.allocate_memory(&allocate_info, None) } {
        Ok(memory) => Ok(memory),
        Err(error) => {
            close_unimported_fd(vulkan_fd, import_context);
            Err(anyhow::anyhow!(
                "{import_context}: vkAllocateMemory failed for DMA-BUF import: {error:?}"
            ))
        }
    }
}

/// Привязывает imported memory к image и чистит Vulkan-ресурсы при ошибке bind.
fn bind_imported_memory_to_image(
    raw_device: &ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    memory_offset: u64,
    bind_context: &'static str,
) -> anyhow::Result<()> {
    match unsafe { raw_device.bind_image_memory(image, memory, memory_offset) } {
        Ok(()) => Ok(()),
        Err(error) => {
            unsafe { raw_device.free_memory(memory, None) };
            unsafe { raw_device.destroy_image(image, None) };
            Err(anyhow::anyhow!(
                "{bind_context}: bind_image_memory failed: {error:?}"
            ))
        }
    }
}

/// Логирует первый VA export descriptor на info-уровне для диагностики zero-copy.
fn log_first_export_descriptor(image: &DecodedDmaBufImage, frame_format: DmaBufFrameFormat) {
    static LOGGED_NV12_EXPORT_DESCRIPTOR: AtomicBool = AtomicBool::new(false);
    static LOGGED_P010_EXPORT_DESCRIPTOR: AtomicBool = AtomicBool::new(false);

    let already_logged = match frame_format {
        DmaBufFrameFormat::Nv12 => LOGGED_NV12_EXPORT_DESCRIPTOR.swap(true, Ordering::Relaxed),
        DmaBufFrameFormat::P010 => LOGGED_P010_EXPORT_DESCRIPTOR.swap(true, Ordering::Relaxed),
    };
    if already_logged {
        return;
    }

    let modifiers = image
        .objects
        .iter()
        .map(|object| object.drm_format_modifier)
        .collect::<Vec<_>>();
    let object_sizes = image
        .objects
        .iter()
        .map(|object| object.size)
        .collect::<Vec<_>>();
    let layer_formats = image
        .layers
        .iter()
        .map(|layer| layer.drm_format)
        .collect::<Vec<_>>();
    let plane_counts = image
        .layers
        .iter()
        .map(|layer| layer.num_planes)
        .collect::<Vec<_>>();
    let object_indices = image
        .layers
        .iter()
        .map(|layer| layer.object_index)
        .collect::<Vec<_>>();
    let offsets = image
        .layers
        .iter()
        .map(|layer| layer.offset)
        .collect::<Vec<_>>();
    let pitches = image
        .layers
        .iter()
        .map(|layer| layer.pitch)
        .collect::<Vec<_>>();

    match frame_format {
        DmaBufFrameFormat::Nv12 => tracing::info!(
            fourcc = image.fourcc,
            layout = ?image.export_layout,
            width = image.width,
            height = image.height,
            objects = image.objects.len(),
            layers = image.layers.len(),
            layer_formats = ?layer_formats,
            plane_counts = ?plane_counts,
            object_indices = ?object_indices,
            offsets = ?offsets,
            pitches = ?pitches,
            modifiers = ?modifiers,
            object_sizes = ?object_sizes,
            "First VA NV12 DMA-BUF export descriptor"
        ),
        DmaBufFrameFormat::P010 => tracing::info!(
            fourcc = image.fourcc,
            layout = ?image.export_layout,
            width = image.width,
            height = image.height,
            objects = image.objects.len(),
            layers = image.layers.len(),
            layer_formats = ?layer_formats,
            plane_counts = ?plane_counts,
            object_indices = ?object_indices,
            offsets = ?offsets,
            pitches = ?pitches,
            modifiers = ?modifiers,
            object_sizes = ?object_sizes,
            "First VA P010 DMA-BUF export descriptor"
        ),
    }
}

/// Импортёр DMA-BUF fd в wgpu textures.
///
/// Хранит wgpu handles и при каждом вызове получает raw Vulkan handles через `as_hal()`.
pub struct DmaBufImporter {
    device: wgpu::Device,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
}

/// Владелец wgpu textures, созданных из exported DMA-BUF.
#[derive(Clone)]
pub(crate) enum ImportedDmaBufStorage {
    /// Один multi-planar texture для composed NV12/P010 descriptor-а.
    Multiplanar(wgpu::Texture),

    /// Два отдельных texture для VA separate-layer descriptor-а.
    SeparatePlanes {
        /// Luma plane texture.
        y_texture: wgpu::Texture,
        /// Interleaved chroma plane texture.
        uv_texture: wgpu::Texture,
    },
}

/// Результат zero-copy импорта multi-planar DMA-BUF.
///
/// Хранит texture storage и два typed plane view.
pub(crate) struct ImportedDmaBufTexture {
    /// Формат decoded frame, выбранный из DRM fourcc descriptor-а.
    pub(crate) frame_format: DmaBufFrameFormat,

    /// Imported texture storage, который владеет raw VkImage/VkDeviceMemory.
    pub(crate) storage: ImportedDmaBufStorage,

    /// View первой plane: luma/Y.
    pub(crate) y_view: wgpu::TextureView,

    /// View второй plane: interleaved chroma/UV.
    pub(crate) uv_view: wgpu::TextureView,
}

impl DmaBufImporter {
    /// Создаёт импортёр.
    pub fn new(device: wgpu::Device, instance: wgpu::Instance, adapter: wgpu::Adapter) -> Self {
        Self {
            device,
            instance,
            adapter,
        }
    }

    /// Импортирует NV12 frame (две плоскости: Y + UV) в пару wgpu textures.
    ///
    /// # Аргументы
    /// * `video_frame` — `Arc<GenericDmaVideoFrame>` от cros-codecs decoded handle.
    ///
    /// # Возвращаемое значение
    /// `(y_texture, uv_texture)` — wgpu textures для биндинга в шейдер.
    pub fn import_nv12(
        &self,
        video_frame: &Arc<GenericDmaVideoFrame>,
    ) -> anyhow::Result<(wgpu::Texture, wgpu::Texture)> {
        // Клонируем frame для получения VA surface descriptor.
        // Клон дублирует fd (dup), так что descriptor будет валиден пока frame жив.
        let mut frame = (**video_frame).clone();
        let desc = frame.va_surface_attribute();

        // Для NV12 ожидаем один dma-buf объект с двумя плоскостями.
        let fd = desc.objects[0].fd;
        let width = desc.width;
        let height = desc.height;

        // Dup fd — оригинал закроется при drop frame, а этот fd закрывается
        // в конце функции или при раннем выходе через явный cleanup ниже.
        let fd_dup =
            nix::unistd::dup(fd).with_context(|| format!("dup dma-buf fd {} failed", fd))?;

        let y_offset = desc.layers[0].offset[0] as u64;
        let uv_offset = desc.layers[0].offset[1] as u64;
        let y_pitch = desc.layers[0].pitch[0] as u32;
        let uv_pitch = desc.layers[0].pitch[1] as u32;
        let modifier = desc.objects[0].drm_format_modifier;
        tracing::debug!(
            width,
            height,
            y_offset,
            uv_offset,
            y_pitch,
            uv_pitch,
            modifier,
            "DMA-BUF desc for import"
        );

        // Импортируем Y-плоскость.
        let y_texture = self
            .import_plane(
                fd_dup,
                y_offset,
                width,
                height,
                y_pitch,
                modifier,
                wgpu::TextureFormat::R8Unorm,
            )
            .with_context(|| "Y-plane DMA-BUF import failed");
        let y_texture = match y_texture {
            Ok(texture) => texture,
            Err(error) => {
                close_unimported_fd(fd_dup, "NV12 Y-plane import cleanup");
                return Err(error);
            }
        };

        // Для UV-плоскости dup fd ещё раз (каждый allocate_memory может или dup, или consume fd).
        let fd_dup2 =
            match nix::unistd::dup(fd_dup).with_context(|| "dup dma-buf fd for UV plane failed") {
                Ok(duplicated_fd) => duplicated_fd,
                Err(error) => {
                    close_unimported_fd(fd_dup, "NV12 UV-plane fd duplication cleanup");
                    return Err(error);
                }
            };

        // Импортируем UV-плоскость.
        let uv_texture = self
            .import_plane(
                fd_dup2,
                uv_offset,
                width / 2,
                height / 2,
                uv_pitch,
                modifier,
                wgpu::TextureFormat::Rg8Unorm,
            )
            .with_context(|| "UV-plane DMA-BUF import failed");
        let uv_texture = match uv_texture {
            Ok(texture) => texture,
            Err(error) => {
                close_unimported_fd(fd_dup, "NV12 UV-plane import cleanup");
                close_unimported_fd(fd_dup2, "NV12 UV-plane import cleanup");
                return Err(error);
            }
        };

        // Закрываем только caller-owned dup fds. Внутренние fd, переданные
        // в `vkAllocateMemory`, закрывает Vulkan implementation после успешного import.
        close_unimported_fd(fd_dup, "NV12 imported Y-plane caller fd cleanup");
        close_unimported_fd(fd_dup2, "NV12 imported UV-plane caller fd cleanup");

        Ok((y_texture, uv_texture))
    }

    /// Импортирует DMA-BUF, экспортированный напрямую из decoded VA surface.
    ///
    /// Decoder остаётся на internal VA surfaces, а готовая поверхность экспортируется через
    /// `vaExportSurfaceHandle()`. Драйвер может вернуть composed multi-planar image или
    /// отдельные luma/chroma layers; оба варианта остаются zero-copy.
    pub(crate) fn import_exported_dma_buf_image(
        &self,
        image: &DecodedDmaBufImage,
    ) -> anyhow::Result<ImportedDmaBufTexture> {
        match image.export_layout {
            DecodedDmaBufExportLayout::ComposedLayers => {
                let layer = image
                    .layers
                    .first()
                    .context("exported DMA-BUF image has no DRM PRIME layers")?;
                if layer.num_planes < 2 {
                    anyhow::bail!(
                        "exported DMA-BUF layer has fewer than 2 planes: {}",
                        layer.num_planes
                    );
                }

                let frame_format = DmaBufFrameFormat::from_fourcc(image.fourcc, layer.drm_format)?;

                log_first_export_descriptor(image, frame_format);

                self.import_multiplanar_dma_buf(image, layer, frame_format)
                    .with_context(|| {
                        format!(
                            "multi-planar exported VA {} surface import failed",
                            frame_format.diagnostic_label()
                        )
                    })
            }
            DecodedDmaBufExportLayout::SeparateLayers => {
                let frame_format =
                    DmaBufFrameFormat::from_separate_layers(image.fourcc, &image.layers)?;

                log_first_export_descriptor(image, frame_format);

                self.import_separate_layer_dma_buf(image, frame_format)
                    .with_context(|| {
                        format!(
                            "separate-layer exported VA {} surface import failed",
                            frame_format.diagnostic_label()
                        )
                    })
            }
        }
    }

    /// Импортирует exported VA descriptor как один Vulkan multi-planar image.
    ///
    /// Важная деталь: tiled/non-linear NV12/P010 нельзя безопасно импортировать как
    /// две независимые картинки. DRM modifier описывает layout всего
    /// multi-planar image, поэтому обе plane layout передаются в один
    /// `VkImageDrmFormatModifierExplicitCreateInfoEXT`.
    fn import_multiplanar_dma_buf(
        &self,
        image: &DecodedDmaBufImage,
        layer: &cros_codecs::decoder::DecodedDmaBufLayer,
        frame_format: DmaBufFrameFormat,
    ) -> anyhow::Result<ImportedDmaBufTexture> {
        frame_format.ensure_device_feature(self.device.features())?;

        if layer.object_index[0] != layer.object_index[1] {
            anyhow::bail!(
                "multi-object {} DMA-BUF export is not implemented yet: object_index={:?}",
                frame_format.diagnostic_label(),
                layer.object_index
            );
        }

        let object_index = layer.object_index[0] as usize;
        let object = image.objects.get(object_index).ok_or_else(|| {
            anyhow::anyhow!(
                "exported {} layer references missing object {}",
                frame_format.diagnostic_label(),
                object_index
            )
        })?;

        let modifier = object.drm_format_modifier;
        let use_drm_modifier = modifier != DRM_FORMAT_MOD_LINEAR;
        let tiling = if use_drm_modifier {
            tracing::debug!(
                modifier,
                format = frame_format.diagnostic_label(),
                "Using VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT for multi-planar DMA-BUF import"
            );
            vk::ImageTiling::from_raw(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT)
        } else {
            tracing::debug!(
                format = frame_format.diagnostic_label(),
                "Using VK_IMAGE_TILING_LINEAR for linear multi-planar DMA-BUF import"
            );
            vk::ImageTiling::LINEAR
        };

        let y_layout = vk::SubresourceLayout::default()
            .offset(u64::from(layer.offset[0]))
            .row_pitch(u64::from(layer.pitch[0]))
            .size(0)
            .array_pitch(0)
            .depth_pitch(0);
        let uv_layout = vk::SubresourceLayout::default()
            .offset(u64::from(layer.offset[1]))
            .row_pitch(u64::from(layer.pitch[1]))
            .size(0)
            .array_pitch(0)
            .depth_pitch(0);
        let plane_layouts = [y_layout, uv_layout];

        let hal_device = unsafe { self.device.as_hal::<wgpu::hal::vulkan::Api>() }
            .context("Zero-copy import requires Vulkan backend")?;
        let raw_device = hal_device.raw_device();

        let hal_instance = unsafe { self.instance.as_hal::<wgpu::hal::vulkan::Api>() }
            .context("Zero-copy import requires Vulkan backend")?;
        let raw_instance = hal_instance.shared_instance().raw_instance();

        let hal_adapter = unsafe { self.adapter.as_hal::<wgpu::hal::vulkan::Api>() }
            .context("Zero-copy import requires Vulkan backend")?;
        let physical_device = hal_adapter.raw_physical_device();

        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let mut drm_modifier_info = if use_drm_modifier {
            Some(
                vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
                    .drm_format_modifier(modifier)
                    .plane_layouts(&plane_layouts),
            )
        } else {
            None
        };

        let mut image_info = vk::ImageCreateInfo::default()
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
            .image_type(vk::ImageType::TYPE_2D)
            .format(frame_format.vulkan_texture_format())
            .extent(vk::Extent3D {
                width: image.width,
                height: image.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(tiling)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::PREINITIALIZED)
            .push_next(&mut external_memory_info);

        if let Some(ref mut drm_info) = drm_modifier_info {
            image_info = image_info.push_next(drm_info);
        }

        let vk_image = unsafe { raw_device.create_image(&image_info, None)? };
        let mem_requirements = unsafe { raw_device.get_image_memory_requirements(vk_image) };
        let mem_properties =
            unsafe { raw_instance.get_physical_device_memory_properties(physical_device) };

        let memory_type_index = match (0..mem_properties.memory_type_count)
            .find(|i| {
                let type_bits = mem_requirements.memory_type_bits;
                type_bits & (1 << i) != 0
                    && mem_properties.memory_types[*i as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .with_context(|| {
                format!(
                    "No suitable Vulkan memory type for multi-planar {} DMA-BUF import",
                    frame_format.diagnostic_label()
                )
            }) {
            Ok(memory_type_index) => memory_type_index as u32,
            Err(error) => {
                unsafe { raw_device.destroy_image(vk_image, None) };
                return Err(error);
            }
        };

        let memory = match allocate_dma_buf_memory_for_image(
            raw_device,
            vk_image,
            mem_requirements,
            memory_type_index,
            object.fd.as_raw_fd(),
            "multi-planar DMA-BUF import",
        ) {
            Ok(memory) => memory,
            Err(error) => {
                unsafe { raw_device.destroy_image(vk_image, None) };
                return Err(error);
            }
        };

        bind_imported_memory_to_image(
            raw_device,
            vk_image,
            memory,
            0,
            "multi-planar DMA-BUF import",
        )?;

        let raw_device_clone = raw_device.clone();
        let drop_callback: Option<wgpu::hal::DropCallback> = Some(Box::new(move || {
            tracing::trace!("Destroying imported multi-planar Vulkan image and memory");
            unsafe { raw_device_clone.destroy_image(vk_image, None) };
            unsafe { raw_device_clone.free_memory(memory, None) };
        }));

        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some(frame_format.texture_label()),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: frame_format.wgpu_texture_format(),
            usage: wgpu_types::TextureUses::RESOURCE,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: vec![],
        };

        let hal_texture = unsafe {
            hal_device.texture_from_raw(
                vk_image,
                &hal_desc,
                drop_callback,
                wgpu::hal::vulkan::TextureMemory::External,
            )
        };

        let wgpu_desc = wgpu::TextureDescriptor {
            label: Some(frame_format.texture_label()),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: frame_format.wgpu_texture_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };

        let texture = unsafe {
            self.device
                .create_texture_from_hal::<wgpu::hal::vulkan::Api>(hal_texture, &wgpu_desc)
        };

        let view_contract = plane_view_contract_for_imported_format(frame_format);
        let y_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(frame_format.y_view_label()),
            format: Some(view_contract.y_plane.format),
            aspect: view_contract.y_plane.aspect,
            ..Default::default()
        });
        let uv_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(frame_format.uv_view_label()),
            format: Some(view_contract.uv_plane.format),
            aspect: view_contract.uv_plane.aspect,
            ..Default::default()
        });

        Ok(ImportedDmaBufTexture {
            frame_format,
            storage: ImportedDmaBufStorage::Multiplanar(texture),
            y_view,
            uv_view,
        })
    }

    /// Импортирует VA separate-layer descriptor как два отдельных Vulkan images.
    ///
    /// Это основной проверенный path для Intel i965 P010: driver отдаёт zero-copy
    /// descriptor как `R16` luma + `GR32` interleaved chroma.
    fn import_separate_layer_dma_buf(
        &self,
        image: &DecodedDmaBufImage,
        frame_format: DmaBufFrameFormat,
    ) -> anyhow::Result<ImportedDmaBufTexture> {
        frame_format.ensure_separate_layer_device_features(self.device.features())?;

        let y_layer = image
            .layers
            .first()
            .context("separate-layer DMA-BUF image has no luma layer")?;
        let uv_layer = image
            .layers
            .get(1)
            .context("separate-layer DMA-BUF image has no chroma layer")?;

        let y_object = image
            .objects
            .get(y_layer.object_index[0] as usize)
            .context("separate-layer luma layer references missing DMA-BUF object")?;
        let uv_object = image
            .objects
            .get(uv_layer.object_index[0] as usize)
            .context("separate-layer chroma layer references missing DMA-BUF object")?;

        let view_contract = plane_view_contract_for_imported_format(frame_format);
        tracing::debug!(
            format = frame_format.diagnostic_label(),
            y_modifier = y_object.drm_format_modifier,
            uv_modifier = uv_object.drm_format_modifier,
            "Importing separate-layer VA DMA-BUF descriptor"
        );

        let y_texture = self
            .import_plane(
                y_object.fd.as_raw_fd(),
                u64::from(y_layer.offset[0]),
                image.width,
                image.height,
                y_layer.pitch[0],
                y_object.drm_format_modifier,
                view_contract.y_plane.format,
            )
            .context("separate-layer luma DMA-BUF import failed")?;

        let uv_texture = self
            .import_plane(
                uv_object.fd.as_raw_fd(),
                u64::from(uv_layer.offset[0]),
                image.width / 2,
                image.height / 2,
                uv_layer.pitch[0],
                uv_object.drm_format_modifier,
                view_contract.uv_plane.format,
            )
            .context("separate-layer chroma DMA-BUF import failed")?;

        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(frame_format.y_view_label()),
            format: Some(view_contract.y_plane.format),
            ..Default::default()
        });
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(frame_format.uv_view_label()),
            format: Some(view_contract.uv_plane.format),
            ..Default::default()
        });

        Ok(ImportedDmaBufTexture {
            frame_format,
            storage: ImportedDmaBufStorage::SeparatePlanes {
                y_texture,
                uv_texture,
            },
            y_view,
            uv_view,
        })
    }

    /// Импортирует одну плоскость (fd + offset) в wgpu texture.
    ///
    /// # Аргументы
    /// * `fd` — dup'd dma-buf fd.
    /// * `offset` — offset внутри dma-buf (должен быть aligned к memory requirements).
    /// * `width`, `height` — размеры плоскости.
    /// * `pitch` — row pitch плоскости в байтах.
    /// * `modifier` — DRM format modifier из dma-buf descriptor.
    /// * `format` — plane texture format (`R8/Rg8` или `R16/Rg16`).
    fn import_plane(
        &self,
        fd: RawFd,
        offset: u64,
        width: u32,
        height: u32,
        pitch: u32,
        modifier: u64,
        format: wgpu::TextureFormat,
    ) -> anyhow::Result<wgpu::Texture> {
        // Получаем HAL-level Vulkan handles через wgpu's `as_hal()` API.
        let hal_device = unsafe { self.device.as_hal::<wgpu::hal::vulkan::Api>() }
            .context("Zero-copy import requires Vulkan backend")?;
        let raw_device = hal_device.raw_device();

        let hal_instance = unsafe { self.instance.as_hal::<wgpu::hal::vulkan::Api>() }
            .context("Zero-copy import requires Vulkan backend")?;
        let raw_instance = hal_instance.shared_instance().raw_instance();

        let hal_adapter = unsafe { self.adapter.as_hal::<wgpu::hal::vulkan::Api>() }
            .context("Zero-copy import requires Vulkan backend")?;
        let physical_device = hal_adapter.raw_physical_device();

        // 1. Создаём VkImage с external memory handle type.
        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let vk_format = match format {
            wgpu::TextureFormat::R8Unorm => vk::Format::R8_UNORM,
            wgpu::TextureFormat::Rg8Unorm => vk::Format::R8G8_UNORM,
            wgpu::TextureFormat::R16Unorm => vk::Format::R16_UNORM,
            wgpu::TextureFormat::Rg16Unorm => vk::Format::R16G16_UNORM,
            _ => anyhow::bail!("Unsupported format for DMA-BUF import: {:?}", format),
        };

        // Определяем tiling в зависимости от modifier.
        // Если modifier != 0 (не linear), ОБЯЗАТЕЛЬНО используем VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT
        // и передаём модификатор через VkImageDrmFormatModifierExplicitCreateInfoEXT.
        // Иначе драйвер прочитает tiled memory как linear — результат: зелёный экран.
        let use_drm_modifier = modifier != DRM_FORMAT_MOD_LINEAR;
        let tiling = if use_drm_modifier {
            tracing::debug!(
                modifier,
                "Using VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT for DMA-BUF import"
            );
            vk::ImageTiling::from_raw(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT)
        } else {
            tracing::debug!("Using VK_IMAGE_TILING_LINEAR for DMA-BUF import (modifier=0)");
            vk::ImageTiling::LINEAR
        };

        // Подготавливаем plane layout для DRM modifier.
        // size=0 требуется спецификацией (драйвер вычисляет сам).
        let plane_layout = vk::SubresourceLayout::default()
            .offset(offset)
            .row_pitch(pitch as u64)
            .size(0)
            .array_pitch(0)
            .depth_pitch(0);

        // Если используем DRM modifier, создаём explicit create info.
        // drmFormatModifierPlaneCount=1, т.к. каждая плоскость импортируется как отдельный VkImage.
        let mut drm_modifier_info = if use_drm_modifier {
            Some(
                vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
                    .drm_format_modifier(modifier)
                    .plane_layouts(std::slice::from_ref(&plane_layout)),
            )
        } else {
            None
        };

        let mut image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(tiling)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            // PREINITIALIZED означает что данные уже инициализированы вне Vulkan
            // (VA-API decoder записал их в DMA-BUF). Это предотвращает потерю данных
            // при layout transition из UNDEFINED.
            .initial_layout(vk::ImageLayout::PREINITIALIZED)
            .push_next(&mut external_memory_info);

        // Добавляем DRM modifier info в pNext chain если нужно.
        if let Some(ref mut drm_info) = drm_modifier_info {
            image_info = image_info.push_next(drm_info);
        }

        let image = unsafe { raw_device.create_image(&image_info, None)? };

        // 2. Получаем memory requirements для image.
        let mem_requirements = unsafe { raw_device.get_image_memory_requirements(image) };
        tracing::trace!(
            width,
            height,
            ?format,
            size = mem_requirements.size,
            alignment = mem_requirements.alignment,
            offset,
            "Vulkan memory requirements"
        );

        // 3. Находим подходящий memory type (device-local).
        let mem_properties =
            unsafe { raw_instance.get_physical_device_memory_properties(physical_device) };

        let memory_type_index = match (0..mem_properties.memory_type_count)
            .find(|i| {
                let type_bits = mem_requirements.memory_type_bits;
                type_bits & (1 << i) != 0
                    && mem_properties.memory_types[*i as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .context("No suitable Vulkan memory type for DMA-BUF import")
        {
            Ok(memory_type_index) => memory_type_index as u32,
            Err(error) => {
                unsafe { raw_device.destroy_image(image, None) };
                return Err(error);
            }
        };

        // 4. Dedicated allocate info — КРИТИЧНО для DMA-BUF image import.
        // Без этого драйвер может неправильно ассоциировать память с image,
        // что приводит к чтению мусора (зелёный экран).
        let memory = match allocate_dma_buf_memory_for_image(
            raw_device,
            image,
            mem_requirements,
            memory_type_index,
            fd,
            "single-plane DMA-BUF import",
        ) {
            Ok(memory) => memory,
            Err(error) => {
                unsafe { raw_device.destroy_image(image, None) };
                return Err(error);
            }
        };

        // 5. Привязываем image к imported memory.
        //
        // Для DRM modifier layout offset уже передан через SubresourceLayout.
        // Повторять тот же offset в vkBindImageMemory нельзя: это сдвинет image
        // второй раз относительно DMA-BUF payload. Для linear fallback offset
        // остаётся единственным способом выбрать нужную плоскость.
        let memory_bind_offset = if use_drm_modifier { 0 } else { offset };
        bind_imported_memory_to_image(
            raw_device,
            image,
            memory,
            memory_bind_offset,
            "single-plane DMA-BUF import",
        )?;

        // 6. Callback для cleanup при drop wgpu texture.
        let raw_device_clone = raw_device.clone();
        let drop_callback: Option<wgpu::hal::DropCallback> = Some(Box::new(move || {
            // SAFETY: image и memory валидны до вызова callback.
            tracing::trace!("Destroying imported Vulkan image and memory");
            unsafe { raw_device_clone.destroy_image(image, None) };
            unsafe { raw_device_clone.free_memory(memory, None) };
        }));

        // 7. Создаём HAL texture wrapper.
        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some("dma-buf-imported"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu_types::TextureUses::RESOURCE,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: vec![],
        };

        let hal_texture = unsafe {
            hal_device.texture_from_raw(
                image,
                &hal_desc,
                drop_callback,
                wgpu::hal::vulkan::TextureMemory::External,
            )
        };

        // 8. Обертываем HAL texture в wgpu::Texture.
        let wgpu_desc = wgpu::TextureDescriptor {
            label: Some("dma-buf-imported"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };

        let texture = unsafe {
            self.device
                .create_texture_from_hal::<wgpu::hal::vulkan::Api>(hal_texture, &wgpu_desc)
        };

        Ok(texture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что P010 import не разрешается без wgpu feature gate.
    #[test]
    fn p010_import_requires_texture_format_p010_feature() {
        let error = DmaBufFrameFormat::P010
            .ensure_device_feature(wgpu::Features::TEXTURE_FORMAT_NV12)
            .unwrap_err();

        assert!(
            error.to_string().contains("TEXTURE_FORMAT_P010"),
            "unexpected error: {error}"
        );
    }

    /// Проверяет, что P010 descriptor выбирает именно P010 import contract.
    #[test]
    fn p010_fourcc_maps_to_dma_buf_frame_format() {
        let frame_format = DmaBufFrameFormat::from_fourcc(DRM_FORMAT_P010, DRM_FORMAT_P010)
            .expect("P010 fourcc must be supported");

        assert_eq!(frame_format, DmaBufFrameFormat::P010);
        assert_eq!(
            frame_format.wgpu_texture_format(),
            wgpu::TextureFormat::P010
        );
    }

    /// Проверяет P010 descriptor в форме VA separate layers: R16 + GR32.
    #[test]
    fn separate_layer_p010_maps_to_dma_buf_frame_format() {
        let layers = vec![
            DecodedDmaBufLayer {
                drm_format: DRM_FORMAT_R16,
                num_planes: 1,
                object_index: [0, 0, 0, 0],
                offset: [0, 0, 0, 0],
                pitch: [7680, 0, 0, 0],
            },
            DecodedDmaBufLayer {
                drm_format: DRM_FORMAT_GR1616,
                num_planes: 1,
                object_index: [0, 0, 0, 0],
                offset: [16_588_800, 0, 0, 0],
                pitch: [7680, 0, 0, 0],
            },
        ];

        let frame_format = DmaBufFrameFormat::from_separate_layers(DRM_FORMAT_P010, &layers)
            .expect("separate-layer P010 descriptor must be supported");

        assert_eq!(frame_format, DmaBufFrameFormat::P010);
    }

    /// Проверяет, что separate-layer P010 требует 16-bit normalized plane formats.
    #[test]
    fn separate_layer_p010_requires_16bit_norm_feature() {
        let error = DmaBufFrameFormat::P010
            .ensure_separate_layer_device_features(wgpu::Features::TEXTURE_FORMAT_NV12)
            .unwrap_err();

        assert!(
            error.to_string().contains("TEXTURE_FORMAT_16BIT_NORM"),
            "unexpected error: {error}"
        );

        DmaBufFrameFormat::P010
            .ensure_separate_layer_device_features(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
            .unwrap();
    }

    /// Проверяет plane views для P010: plane0/plane1 и 16-bit normalized formats.
    #[test]
    fn imported_p010_views_use_plane_aspects_and_16bit_formats() {
        let contract = plane_view_contract_for_imported_format(DmaBufFrameFormat::P010);

        assert_eq!(contract.texture_format, wgpu::TextureFormat::P010);
        assert_eq!(contract.y_plane.aspect, wgpu::TextureAspect::Plane0);
        assert_eq!(contract.y_plane.format, wgpu::TextureFormat::R16Unorm);
        assert_eq!(contract.uv_plane.aspect, wgpu::TextureAspect::Plane1);
        assert_eq!(contract.uv_plane.format, wgpu::TextureFormat::Rg16Unorm);
    }
}
