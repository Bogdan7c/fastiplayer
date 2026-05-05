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
use std::os::fd::RawFd;
use std::sync::Arc;

use anyhow::Context;
use ash::vk;
use cros_codecs::libva::ExternalBufferDescriptor;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;

/// Значение VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT из Vulkan headers.
/// ash 0.38 не экспортирует эту константу напрямую.
const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i32 = 1000158000;

/// Значение DRM_FORMAT_MOD_LINEAR = 0 (linear, untiled).
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Импортёр DMA-BUF fd в wgpu textures.
///
/// Хранит wgpu handles и при каждом вызове получает raw Vulkan handles через `as_hal()`.
pub struct DmaBufImporter {
    device: wgpu::Device,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
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

        // Dup fd — оригинал закроется при drop frame.
        let fd_dup =
            nix::unistd::dup(fd).with_context(|| format!("dup dma-buf fd {} failed", fd))?;

        let y_offset = desc.layers[0].offset[0] as u64;
        let uv_offset = desc.layers[0].offset[1] as u64;
        let y_pitch = desc.layers[0].pitch[0] as u32;
        let uv_pitch = desc.layers[0].pitch[1] as u32;
        let modifier = desc.objects[0].drm_format_modifier;
        tracing::info!(
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
            .with_context(|| "Y-plane DMA-BUF import failed")?;

        // Для UV-плоскости dup fd ещё раз (каждый allocate_memory может или dup, или consume fd).
        let fd_dup2 =
            nix::unistd::dup(fd_dup).with_context(|| "dup dma-buf fd for UV plane failed")?;

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
            .with_context(|| "UV-plane DMA-BUF import failed")?;

        // Закрываем dup fds. Vulkan уже скопировал/принял ownership в allocate_memory.
        // fd_dup2 был передан в import_plane, который сделал ещё один dup (vk_fd)
        // и закрыл его после allocate_memory.
        let _ = nix::unistd::close(fd_dup);
        let _ = nix::unistd::close(fd_dup2);

        Ok((y_texture, uv_texture))
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
            tracing::info!(
                modifier,
                "Using VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT for DMA-BUF import"
            );
            vk::ImageTiling::from_raw(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT)
        } else {
            tracing::info!("Using VK_IMAGE_TILING_LINEAR for DMA-BUF import (modifier=0)");
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
        tracing::info!(
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

        let memory_type_index = (0..mem_properties.memory_type_count)
            .find(|i| {
                let type_bits = mem_requirements.memory_type_bits;
                type_bits & (1 << i) != 0
                    && mem_properties.memory_types[*i as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .context("No suitable Vulkan memory type for DMA-BUF import")?
            as u32;

        // 4. Dup fd для Vulkan (спецификация позволяет закрыть fd сразу после allocate_memory).
        let vk_fd = nix::unistd::dup(fd).with_context(|| "dup fd for Vulkan import failed")?;

        // Dedicated allocate info — КРИТИЧНО для DMA-BUF image import.
        // Без этого драйвер может неправильно ассоциировать память с image,
        // что приводит к чтению мусора (зелёный экран).
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(image);

        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(vk_fd);

        let allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_requirements.size)
            .memory_type_index(memory_type_index)
            .push_next(&mut dedicated_info)
            .push_next(&mut import_info);

        let memory = unsafe { raw_device.allocate_memory(&allocate_info, None)? };

        // Спецификация Vulkan: application может безопасно закрыть fd после allocate_memory.
        let _ = nix::unistd::close(vk_fd);

        // 5. Привязываем image к imported memory с заданным offset.
        // Offset должен быть aligned к mem_requirements.alignment.
        unsafe {
            raw_device
                .bind_image_memory(image, memory, offset)
                .with_context(|| {
                    format!(
                        "bind_image_memory failed: offset={} may be unaligned (alignment={})",
                        offset, mem_requirements.alignment
                    )
                })?;
        }

        // 6. Callback для cleanup при drop wgpu texture.
        let raw_device_clone = raw_device.clone();
        let drop_callback: Option<wgpu::hal::DropCallback> = Some(Box::new(move || {
            // SAFETY: image и memory валидны до вызова callback.
            tracing::info!("Destroying imported Vulkan image and memory");
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
