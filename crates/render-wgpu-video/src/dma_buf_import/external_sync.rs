//! Явная синхронизация «внешний писатель (VA-API/VCN) → Vulkan sampler» для
//! импортированного DMA-BUF image, который живёт дольше одного кадра.
//!
//! Зачем. Пока image импортировался на каждый кадр, первый барьер ставил сам
//! wgpu: `UNDEFINED -> SHADER_READ_ONLY_OPTIMAL` при первом использовании.
//! При переиспользовании image (cpu-parity сессия 03) tracker wgpu считает, что
//! image уже в `SHADER_READ_ONLY_OPTIMAL`, и барьера не выдаёт, хотя память в
//! это время заново писал аппаратный декодер. Значит, владение и видимость
//! памяти оформляем мы, по спецификации Vulkan «Queue Family Ownership Transfer»:
//!
//! - перед каждым проходом рендера — **acquire**: ownership
//!   `VK_QUEUE_FAMILY_FOREIGN_EXT -> наша graphics family`, `dstAccess = SHADER_READ`,
//!   layout `SHADER_READ_ONLY_OPTIMAL -> SHADER_READ_ONLY_OPTIMAL` (без перехода);
//! - после прохода — **release**: `graphics family -> FOREIGN`, `srcAccess = SHADER_READ`.
//!   Release обязателен: декодер читает эту же поверхность как опорный кадр, а
//!   спецификация гарантирует содержимое другой очереди только после передачи
//!   владения;
//! - сразу после импорта — **инициализация**: acquire `UNDEFINED -> SHADER_READ_ONLY_OPTIMAL`
//!   плюс release обратно, отдельным submit-ом. Единственный разрешённый
//!   `initialLayout` для external memory — `UNDEFINED`
//!   (`VUID-VkImageCreateInfo-pNext-01443`), а после этого image уже никогда не
//!   возвращается в `UNDEFINED`, поэтому «переход из UNDEFINED может потерять
//!   содержимое» касается только момента импорта, как и раньше.
//!
//! Почему `FOREIGN`, а не `EXTERNAL`: `VK_QUEUE_FAMILY_FOREIGN_EXT` по описанию
//! расширения предназначен для «устройств других вендоров и даже не-Vulkan
//! устройств, например fixed-function обработчиков изображений» — это ровно
//! VCN. Для него нужно расширение `VK_EXT_queue_family_foreign`
//! (`VUID-VkImageMemoryBarrier-srcQueueFamilyIndex-09099` / `-dstQueueFamilyIndex-09100`);
//! shell включает его при создании устройства. Без расширения синхронизация
//! недоступна и импорт остаётся покадровым (fail-closed, решение владельца).
//!
//! Почему отдельные command buffer-ы: wgpu-core 30 запрещает смешивать raw-команды
//! (`CommandEncoder::as_hal_mut`) с обычными в одном encoder-е (паника «Mixing the
//! wgpu encoding API with the raw encoding API»). Поэтому acquire и release
//! пишутся в собственные encoder-ы, а shell отправляет их в одном `queue.submit`
//! до и после основного command buffer-а кадра.
//!
//! Спецификация: `oldLayout` обязан быть текущим layout-ом image
//! (`VUID-VkImageMemoryBarrier-oldLayout-01197`); для acquire игнорируется
//! `srcAccessMask`, для release — `dstAccessMask`. Layout
//! `SHADER_READ_ONLY_OPTIMAL` — это то, во что wgpu-hal 30 переводит
//! `TextureUses::RESOURCE` для color image (`derive_image_layout`), поэтому
//! tracker wgpu и реальный layout совпадают.

use std::ffi::CStr;

use anyhow::Context;
use ash::vk;

use super::DmaBufImporter;

/// Доступность явной синхронизации с внешним писателем на этом устройстве.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalMemorySyncSupport {
    /// `VK_EXT_queue_family_foreign` включено; image можно переиспользовать с барьерами.
    ForeignQueueOwnershipTransfer {
        /// Индекс graphics queue family, которой владеет wgpu.
        queue_family_index: u32,
    },

    /// Расширения нет: image импортируется на каждый кадр, как раньше.
    Unavailable,
}

impl ExternalMemorySyncSupport {
    /// Определяет поддержку для wgpu device (один раз на импортёр) и логирует итог.
    pub(super) fn detect_for_device(device: &wgpu::Device) -> Self {
        // SAFETY: `as_hal` только читает список расширений и индекс queue family;
        // raw device не сохраняется дольше guard-а.
        let support = unsafe { device.as_hal::<wgpu::hal::vulkan::Api>() }
            .map_or(Self::Unavailable, |hal_device| Self::detect(&hal_device));
        tracing::info!(
            external_sync = support.diagnostic_label(),
            "DMA-BUF importer external memory synchronization"
        );
        support
    }

    /// Определяет поддержку по списку включённых расширений устройства wgpu.
    pub(crate) fn detect(hal_device: &wgpu::hal::vulkan::Device) -> Self {
        let has_foreign_queue_family = hal_device
            .enabled_device_extensions()
            .iter()
            .any(|extension| *extension == foreign_queue_family_extension_name());
        if has_foreign_queue_family {
            Self::ForeignQueueOwnershipTransfer {
                queue_family_index: hal_device.queue_family_index(),
            }
        } else {
            Self::Unavailable
        }
    }

    /// `true`, если рендерер может безопасно переиспользовать импортированный image.
    pub(crate) const fn allows_image_reuse(self) -> bool {
        matches!(self, Self::ForeignQueueOwnershipTransfer { .. })
    }

    /// Короткая метка для логов и диагностики.
    pub(crate) const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::ForeignQueueOwnershipTransfer { .. } => "foreign-queue ownership transfer",
            Self::Unavailable => "unavailable (per-frame import)",
        }
    }
}

/// Имя расширения `VK_EXT_queue_family_foreign`, которое shell включает при
/// создании устройства ради переиспользования DMA-BUF image.
#[must_use]
pub const fn foreign_queue_family_extension_name() -> &'static CStr {
    ash::ext::queue_family_foreign::NAME
}

/// Один Vulkan image, которому нужны барьеры (multi-planar — один, separate-layer — два).
#[derive(Debug, Clone, Copy)]
struct SyncedImage {
    /// Raw image; владение остаётся у drop callback-а wgpu texture.
    image: vk::Image,
}

/// Состояние синхронизации одного импортированного DMA-BUF image.
///
/// Хранит только то, что нужно для записи барьеров; сам image и память
/// принадлежат wgpu texture (его drop callback уничтожает их), а этот тип
/// живёт внутри `ImportedDmaBufTexture`, то есть строго не дольше image.
pub struct ImportedImageExternalSync {
    /// Клон ash device для `cmd_pipeline_barrier`.
    raw_device: ash::Device,

    /// Image-ы, покрываемые барьером.
    images: Vec<SyncedImage>,

    /// Graphics queue family wgpu — вторая сторона передачи владения.
    queue_family_index: u32,
}

impl std::fmt::Debug for ImportedImageExternalSync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportedImageExternalSync")
            .field("images", &self.images.len())
            .field("queue_family_index", &self.queue_family_index)
            .finish()
    }
}

/// Какой переход layout-а делает acquire-барьер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquireLayoutTransition {
    /// Первый acquire сразу после импорта: image создан в `UNDEFINED`.
    FromUndefined,

    /// Обычный acquire: image уже в `SHADER_READ_ONLY_OPTIMAL`.
    AlreadySampled,
}

impl ImportedImageExternalSync {
    /// Собирает состояние синхронизации для image-ов одного импорта.
    pub(crate) fn new(
        raw_device: ash::Device,
        images: impl IntoIterator<Item = vk::Image>,
        queue_family_index: u32,
    ) -> Self {
        Self {
            raw_device,
            images: images
                .into_iter()
                .map(|image| SyncedImage { image })
                .collect(),
            queue_family_index,
        }
    }

    /// Переводит только что импортированный image из `UNDEFINED` в sampled layout
    /// и возвращает владение внешней очереди. Выполняется один раз на импорт,
    /// отдельным submit-ом, ДО первого прохода рендера с этим image.
    ///
    /// После этого tracker wgpu (`initial_state = RESOURCE`) и реальный layout
    /// совпадают, а каждый проход рендера делает только acquire/release без
    /// перехода layout-а.
    pub(crate) fn initialize_ownership(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures_to_keep_alive: Vec<wgpu::Texture>,
    ) -> anyhow::Result<()> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dma-buf import ownership initialization"),
        });
        self.record_barriers(
            &mut encoder,
            BarrierDirection::Acquire {
                transition: AcquireLayoutTransition::FromUndefined,
            },
        )
        .context("record initial DMA-BUF acquire barrier")?;
        self.record_barriers(&mut encoder, BarrierDirection::Release)
            .context("record initial DMA-BUF release barrier")?;
        queue.submit([encoder.finish()]);
        // Raw-барьер wgpu не отслеживает: если texture уронят до исполнения этого
        // submit-а, drop callback уничтожит VkImage под работающим GPU. Клоны
        // textures держат image живым до подтверждения завершения работы очереди.
        queue.on_submitted_work_done(move || drop(textures_to_keep_alive));
        Ok(())
    }

    /// Записывает acquire-барьер в отдельный command buffer для submit-а ДО
    /// основного command buffer-а кадра.
    pub(crate) fn record_acquire(
        &self,
        device: &wgpu::Device,
    ) -> anyhow::Result<wgpu::CommandBuffer> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dma-buf frame acquire from foreign queue"),
        });
        self.record_barriers(
            &mut encoder,
            BarrierDirection::Acquire {
                transition: AcquireLayoutTransition::AlreadySampled,
            },
        )?;
        Ok(encoder.finish())
    }

    /// Записывает release-барьер в отдельный command buffer для submit-а ПОСЛЕ
    /// основного command buffer-а кадра.
    pub(crate) fn record_release(
        &self,
        device: &wgpu::Device,
    ) -> anyhow::Result<wgpu::CommandBuffer> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dma-buf frame release to foreign queue"),
        });
        self.record_barriers(&mut encoder, BarrierDirection::Release)?;
        Ok(encoder.finish())
    }

    /// Записывает барьеры всех image-ов этого импорта в raw command buffer encoder-а.
    fn record_barriers(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        direction: BarrierDirection,
    ) -> anyhow::Result<()> {
        let barriers: Vec<vk::ImageMemoryBarrier<'static>> = self
            .images
            .iter()
            .map(|synced| image_memory_barrier(synced.image, self.queue_family_index, direction))
            .collect();
        let (src_stage, dst_stage) = direction.stage_masks();

        // SAFETY: encoder создан на том же device, что и image-ы (см. `new`), и
        // используется только raw-API (без wgpu-команд), как требует wgpu-core;
        // raw command buffer не уничтожается и не хранится дольше callback-а;
        // image-ы живы, пока жив `ImportedDmaBufTexture`, владеющий этим типом.
        let recorded = unsafe {
            encoder.as_hal_mut::<wgpu::hal::vulkan::Api, _, _>(|hal_encoder| {
                let hal_encoder = hal_encoder?;
                // Handle активного command buffer-а только читается; command buffer в
                // состоянии recording (wgpu открыл его для raw-записи), барьеры
                // ссылаются на живые image-ы. Unsafe-блок снаружи покрывает замыкание.
                let command_buffer = hal_encoder.raw_handle();
                self.raw_device.cmd_pipeline_barrier(
                    command_buffer,
                    src_stage,
                    dst_stage,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &barriers,
                );
                Some(())
            })
        };
        recorded.context("wgpu command encoder is not a Vulkan encoder in recording state")
    }
}

/// Собирает синхронизацию для image-ов импорта, если устройство её поддерживает,
/// и сразу выполняет одноразовую инициализацию владения.
pub(super) fn attach_external_sync(
    importer: &DmaBufImporter,
    raw_device: &ash::Device,
    images: impl IntoIterator<Item = vk::Image>,
    textures_to_keep_alive: Vec<wgpu::Texture>,
) -> anyhow::Result<Option<ImportedImageExternalSync>> {
    let ExternalMemorySyncSupport::ForeignQueueOwnershipTransfer { queue_family_index } =
        importer.external_sync_support
    else {
        return Ok(None);
    };
    let external_sync =
        ImportedImageExternalSync::new(raw_device.clone(), images, queue_family_index);
    external_sync
        .initialize_ownership(&importer.device, &importer.queue, textures_to_keep_alive)
        .context("initialize foreign-queue ownership of imported DMA-BUF image")?;
    Ok(Some(external_sync))
}

/// Собирает синхронизацию для двух separate-layer image-ов, доставая raw
/// `VkImage` из уже обёрнутых wgpu textures.
pub(super) fn attach_separate_plane_external_sync(
    importer: &DmaBufImporter,
    y_texture: &wgpu::Texture,
    uv_texture: &wgpu::Texture,
) -> anyhow::Result<Option<ImportedImageExternalSync>> {
    if !importer.external_sync_support.allows_image_reuse() {
        return Ok(None);
    }
    // SAFETY: `as_hal` только читает handle; raw device/texture не переживают guard.
    let raw_device = unsafe { importer.device.as_hal::<wgpu::hal::vulkan::Api>() }
        .map(|hal_device| hal_device.raw_device().clone())
        .context("separate-layer DMA-BUF sync requires Vulkan device")?;
    let raw_images = [y_texture, uv_texture]
        .into_iter()
        .map(|texture| {
            // SAFETY: см. выше; handle image принадлежит wgpu texture и жив, пока жива она.
            unsafe { texture.as_hal::<wgpu::hal::vulkan::Api>() }
                .map(|hal_texture| unsafe { hal_texture.raw_handle() })
                .context("separate-layer DMA-BUF plane is not a Vulkan texture")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    attach_external_sync(
        importer,
        &raw_device,
        raw_images,
        vec![y_texture.clone(), uv_texture.clone()],
    )
}

/// Направление передачи владения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrierDirection {
    /// Внешняя очередь -> graphics family; перед чтением в шейдере.
    Acquire {
        /// Переход layout-а на этом acquire.
        transition: AcquireLayoutTransition,
    },

    /// Graphics family -> внешняя очередь; после чтения в шейдере.
    Release,
}

impl BarrierDirection {
    /// Stage masks `vkCmdPipelineBarrier`.
    ///
    /// Для acquire первая область синхронизации не применяется (это сторона
    /// release), поэтому `TOP_OF_PIPE`; для release не применяется вторая —
    /// `BOTTOM_OF_PIPE`. Чтение image идёт только во fragment shader.
    const fn stage_masks(self) -> (vk::PipelineStageFlags, vk::PipelineStageFlags) {
        match self {
            Self::Acquire { .. } => (
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
            ),
            Self::Release => (
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            ),
        }
    }
}

/// Layout, в котором image живёт между проходами: wgpu-hal 30 переводит
/// `TextureUses::RESOURCE` color image именно в него.
const SAMPLED_LAYOUT: vk::ImageLayout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

/// Строит image memory barrier передачи владения для одного image.
///
/// `aspect_mask = COLOR` для multi-planar image покрывает все planes сразу
/// (спецификация разрешает `COLOR` вместо перечисления `PLANE_i`).
fn image_memory_barrier(
    image: vk::Image,
    queue_family_index: u32,
    direction: BarrierDirection,
) -> vk::ImageMemoryBarrier<'static> {
    let subresource_range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    match direction {
        BarrierDirection::Acquire { transition } => {
            let old_layout = match transition {
                AcquireLayoutTransition::FromUndefined => vk::ImageLayout::UNDEFINED,
                AcquireLayoutTransition::AlreadySampled => SAMPLED_LAYOUT,
            };
            vk::ImageMemoryBarrier::default()
                // srcAccessMask у acquire игнорируется спецификацией.
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(old_layout)
                .new_layout(SAMPLED_LAYOUT)
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(queue_family_index)
                .image(image)
                .subresource_range(subresource_range)
        }
        BarrierDirection::Release => vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::SHADER_READ)
            // dstAccessMask у release игнорируется спецификацией.
            .dst_access_mask(vk::AccessFlags::empty())
            .old_layout(SAMPLED_LAYOUT)
            .new_layout(SAMPLED_LAYOUT)
            .src_queue_family_index(queue_family_index)
            .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
            .image(image)
            .subresource_range(subresource_range),
    }
}

#[cfg(test)]
mod tests {
    use ash::vk::Handle;

    use super::*;

    /// Acquire после импорта стартует из `UNDEFINED`, обычный — из sampled layout;
    /// обе стороны передачи владения всегда FOREIGN <-> наша family.
    #[test]
    fn acquire_barriers_transfer_ownership_from_foreign_queue() {
        let image = vk::Image::from_raw(0x1234);

        let first = image_memory_barrier(
            image,
            3,
            BarrierDirection::Acquire {
                transition: AcquireLayoutTransition::FromUndefined,
            },
        );
        let steady = image_memory_barrier(
            image,
            3,
            BarrierDirection::Acquire {
                transition: AcquireLayoutTransition::AlreadySampled,
            },
        );

        assert_eq!(first.old_layout, vk::ImageLayout::UNDEFINED);
        assert_eq!(steady.old_layout, SAMPLED_LAYOUT);
        for barrier in [first, steady] {
            assert_eq!(barrier.new_layout, SAMPLED_LAYOUT);
            assert_eq!(barrier.src_queue_family_index, vk::QUEUE_FAMILY_FOREIGN_EXT);
            assert_eq!(barrier.dst_queue_family_index, 3);
            assert_eq!(barrier.dst_access_mask, vk::AccessFlags::SHADER_READ);
            assert_eq!(barrier.src_access_mask, vk::AccessFlags::empty());
            assert_eq!(barrier.image, image);
            assert_eq!(
                barrier.subresource_range.aspect_mask,
                vk::ImageAspectFlags::COLOR
            );
        }
    }

    /// Release отдаёт владение обратно FOREIGN без смены layout-а и с
    /// `srcAccess = SHADER_READ`, чтобы чтение шейдера завершилось до записи декодера.
    #[test]
    fn release_barrier_returns_ownership_to_foreign_queue_without_layout_change() {
        let image = vk::Image::from_raw(0x5678);

        let barrier = image_memory_barrier(image, 0, BarrierDirection::Release);

        assert_eq!(barrier.old_layout, SAMPLED_LAYOUT);
        assert_eq!(barrier.new_layout, SAMPLED_LAYOUT);
        assert_eq!(barrier.src_queue_family_index, 0);
        assert_eq!(barrier.dst_queue_family_index, vk::QUEUE_FAMILY_FOREIGN_EXT);
        assert_eq!(barrier.src_access_mask, vk::AccessFlags::SHADER_READ);
        assert_eq!(barrier.dst_access_mask, vk::AccessFlags::empty());
    }

    /// Stage masks: acquire ждёт только fragment shader, release — только его завершения.
    #[test]
    fn stage_masks_cover_fragment_sampling_only() {
        let (acquire_src, acquire_dst) = BarrierDirection::Acquire {
            transition: AcquireLayoutTransition::AlreadySampled,
        }
        .stage_masks();
        let (release_src, release_dst) = BarrierDirection::Release.stage_masks();

        assert_eq!(acquire_src, vk::PipelineStageFlags::TOP_OF_PIPE);
        assert_eq!(acquire_dst, vk::PipelineStageFlags::FRAGMENT_SHADER);
        assert_eq!(release_src, vk::PipelineStageFlags::FRAGMENT_SHADER);
        assert_eq!(release_dst, vk::PipelineStageFlags::BOTTOM_OF_PIPE);
    }

    /// Без расширения переиспользование запрещено (fail-closed).
    #[test]
    fn unavailable_support_forbids_image_reuse() {
        assert!(!ExternalMemorySyncSupport::Unavailable.allows_image_reuse());
        assert!(
            ExternalMemorySyncSupport::ForeignQueueOwnershipTransfer {
                queue_family_index: 0
            }
            .allows_image_reuse()
        );
        assert_eq!(
            foreign_queue_family_extension_name().to_str().ok(),
            Some("VK_EXT_queue_family_foreign")
        );
    }
}
