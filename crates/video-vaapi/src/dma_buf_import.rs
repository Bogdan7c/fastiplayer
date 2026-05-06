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
use cros_codecs::decoder::DecodedDmaBufImage;
use cros_codecs::libva::ExternalBufferDescriptor;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;

/// Значение VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT из Vulkan headers.
/// ash 0.38 не экспортирует эту константу напрямую.
const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i32 = 1000158000;

/// DRM fourcc для NV12 (`'N' 'V' '1' '2'` в little-endian представлении).
const DRM_FORMAT_NV12: u32 = 0x3231_564e;

/// Значение DRM_FORMAT_MOD_LINEAR = 0 (linear, untiled).
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

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
            "Failed to close DMA-BUF fd after failed Vulkan import"
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
fn log_first_export_descriptor(
    image: &DecodedDmaBufImage,
    layer: &cros_codecs::decoder::DecodedDmaBufLayer,
) {
    static LOGGED_EXPORT_DESCRIPTOR: AtomicBool = AtomicBool::new(false);
    if LOGGED_EXPORT_DESCRIPTOR.swap(true, Ordering::Relaxed) {
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

    tracing::info!(
        fourcc = image.fourcc,
        layer_fourcc = layer.drm_format,
        width = image.width,
        height = image.height,
        objects = image.objects.len(),
        planes = layer.num_planes,
        object_index = ?layer.object_index,
        offsets = ?layer.offset,
        pitches = ?layer.pitch,
        modifiers = ?modifiers,
        object_sizes = ?object_sizes,
        "First VA DMA-BUF export descriptor"
    );
}

/// Импортёр DMA-BUF fd в wgpu textures.
///
/// Хранит wgpu handles и при каждом вызове получает raw Vulkan handles через `as_hal()`.
pub struct DmaBufImporter {
    device: wgpu::Device,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
}

/// Результат zero-copy импорта NV12 DMA-BUF.
///
/// Хранит один multi-planar `TextureFormat::NV12` texture и два view,
/// которые совместимы с текущим NV12 shader pipeline (`R8Unorm` + `Rg8Unorm`).
pub struct ImportedNv12Texture {
    /// Единый imported texture, который владеет raw VkImage и imported VkDeviceMemory.
    pub texture: wgpu::Texture,
    /// View первой NV12 plane: luma/Y, формат `R8Unorm`.
    pub y_view: wgpu::TextureView,
    /// View второй NV12 plane: interleaved chroma/UV, формат `Rg8Unorm`.
    pub uv_view: wgpu::TextureView,
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
                let _ = nix::unistd::close(fd_dup);
                return Err(error);
            }
        };

        // Для UV-плоскости dup fd ещё раз (каждый allocate_memory может или dup, или consume fd).
        let fd_dup2 =
            match nix::unistd::dup(fd_dup).with_context(|| "dup dma-buf fd for UV plane failed") {
                Ok(duplicated_fd) => duplicated_fd,
                Err(error) => {
                    let _ = nix::unistd::close(fd_dup);
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
                let _ = nix::unistd::close(fd_dup);
                let _ = nix::unistd::close(fd_dup2);
                return Err(error);
            }
        };

        // Закрываем только caller-owned dup fds. Внутренние fd, переданные
        // в `vkAllocateMemory`, закрывает Vulkan implementation после успешного import.
        let _ = nix::unistd::close(fd_dup);
        let _ = nix::unistd::close(fd_dup2);

        Ok((y_texture, uv_texture))
    }

    /// Импортирует NV12 DMA-BUF, экспортированный напрямую из decoded VA surface.
    ///
    /// Это zero-copy path A: decoder остаётся на internal VA surfaces, а готовая
    /// поверхность экспортируется через `vaExportSurfaceHandle()` и импортируется
    /// в Vulkan как один multi-planar NV12 image.
    pub fn import_exported_nv12(
        &self,
        image: &DecodedDmaBufImage,
    ) -> anyhow::Result<ImportedNv12Texture> {
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

        log_first_export_descriptor(image, layer);

        self.import_multiplanar_nv12(image, layer)
            .context("multi-planar exported VA surface import failed")
    }

    /// Импортирует exported VA NV12 descriptor как один Vulkan multi-planar image.
    ///
    /// Важная деталь: tiled/non-linear NV12 нельзя безопасно импортировать как две
    /// независимые R8/RG8 картинки. DRM modifier описывает layout всего
    /// multi-planar image, поэтому обе plane layout передаются в один
    /// `VkImageDrmFormatModifierExplicitCreateInfoEXT`.
    fn import_multiplanar_nv12(
        &self,
        image: &DecodedDmaBufImage,
        layer: &cros_codecs::decoder::DecodedDmaBufLayer,
    ) -> anyhow::Result<ImportedNv12Texture> {
        if !self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
        {
            anyhow::bail!("wgpu device was created without TEXTURE_FORMAT_NV12 support");
        }

        if image.fourcc != DRM_FORMAT_NV12 || layer.drm_format != DRM_FORMAT_NV12 {
            anyhow::bail!(
                "exported VA surface is not NV12: image_fourcc={:#x}, layer_fourcc={:#x}",
                image.fourcc,
                layer.drm_format
            );
        }

        if layer.object_index[0] != layer.object_index[1] {
            anyhow::bail!(
                "multi-object NV12 DMA-BUF export is not implemented yet: object_index={:?}",
                layer.object_index
            );
        }

        let object_index = layer.object_index[0] as usize;
        let object = image.objects.get(object_index).ok_or_else(|| {
            anyhow::anyhow!(
                "exported NV12 layer references missing object {}",
                object_index
            )
        })?;

        let modifier = object.drm_format_modifier;
        let use_drm_modifier = modifier != DRM_FORMAT_MOD_LINEAR;
        let tiling = if use_drm_modifier {
            tracing::debug!(
                modifier,
                "Using VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT for multi-planar NV12 import"
            );
            vk::ImageTiling::from_raw(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT)
        } else {
            tracing::debug!("Using VK_IMAGE_TILING_LINEAR for linear multi-planar NV12 import");
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
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
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
            .context("No suitable Vulkan memory type for multi-planar NV12 DMA-BUF import")
        {
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
            "multi-planar NV12 DMA-BUF import",
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
            "multi-planar NV12 DMA-BUF import",
        )?;

        let raw_device_clone = raw_device.clone();
        let drop_callback: Option<wgpu::hal::DropCallback> = Some(Box::new(move || {
            tracing::trace!("Destroying imported multi-planar NV12 Vulkan image and memory");
            unsafe { raw_device_clone.destroy_image(vk_image, None) };
            unsafe { raw_device_clone.free_memory(memory, None) };
        }));

        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some("dma-buf-imported-nv12"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::NV12,
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
            label: Some("dma-buf-imported-nv12"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::NV12,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };

        let texture = unsafe {
            self.device
                .create_texture_from_hal::<wgpu::hal::vulkan::Api>(hal_texture, &wgpu_desc)
        };

        let y_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("dma-buf-imported-nv12-y"),
            format: Some(wgpu::TextureFormat::R8Unorm),
            aspect: wgpu::TextureAspect::Plane0,
            ..Default::default()
        });
        let uv_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("dma-buf-imported-nv12-uv"),
            format: Some(wgpu::TextureFormat::Rg8Unorm),
            aspect: wgpu::TextureAspect::Plane1,
            ..Default::default()
        });

        Ok(ImportedNv12Texture {
            texture,
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
    /// * `format` — `R8Unorm` для Y, `Rg8Unorm` для UV.
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
