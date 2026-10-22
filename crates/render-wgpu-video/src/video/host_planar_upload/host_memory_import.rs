//! Импорт памяти кадра декодера в Vulkan без CPU-копии (cpu-parity 08b).
//!
//! FFmpeg-декодер (`video-ffmpeg`, свой `get_buffer2`) кладёт каждый кадр в блок обычной
//! памяти процесса, выровненный на 2 МиБ. Здесь блок импортируется в `VkBuffer` через
//! `VK_EXT_external_memory_host`, и GPU копирует plane-ы в текстуры слота прямо из него —
//! так же, как из staging, только без memcpy на главном потоке.
//!
//! **Импорт на каждую GPU-копию, без кэша.** Замер на Radeon 780M (RADV, amdgpu): импорт
//! host-памяти — это `userptr`-буфер, и ядро проверяет страницы КАЖДОГО живого такого
//! буфера на каждом submit (RADV кладёт в submit все буферы устройства). Кэш импортов по
//! блокам (~25 живых 4K-блоков) стоил главному потоку 4,2 % ядра в system time на VP9
//! 4K60, импорт на кадр — 2,5 % (с блоками по 2 МиБ — 1,5 %), при базе со staging 1,0 %.
//! Поэтому импорт живёт ровно столько, сколько его GPU-копия.
//!
//! Время жизни (см. `video_core::HostImportableMemoryBlock`): на каждую копию создаётся
//! `ImportedHostBuffer` (`VkBuffer` + `VkDeviceMemory` над страницами блока) и обёртка
//! `wgpu::Buffer` над ним. Drop-callback обёртки держит импорт (страницы не освободятся,
//! пока на них ссылается Vulkan) и дескриптор кадра (декодер не перепишет блок, пока GPU
//! его читает). wgpu вызывает callback, только когда выполнился последний submit, который
//! читал обёртку, — неважно, ушла копия с кадром или отдельным submit-ом.
//!
//! Fail-closed: нет расширения, драйвер требует выравнивания больше страницы, нет
//! coherent memory type, ошибка Vulkan — кадр идёт обычным путём через staging. Ошибка
//! импорта выключает импорт до конца жизни устройства (такие ошибки системные: драйвер,
//! память), причина пишется в лог один раз. Так путь безопасен и на дискретных
//! видеокартах, и на драйверах, которые мы не проверяли.

use std::ffi::{CStr, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, ensure};
use ash::vk;
use video_core::{HOST_IMPORTABLE_BLOCK_ALIGNMENT, HostImportableMemoryBlock};

use super::direct_upload::DirectHostPlanarUpload;

/// Имя расширения Vulkan, которое включает shell при создании устройства.
pub(crate) fn host_memory_import_extension_name() -> &'static CStr {
    ash::ext::external_memory_host::NAME
}

/// Тип handle-а импортируемой памяти: обычная память процесса.
const HOST_ALLOCATION: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;

/// Счётчики импортёра (тесты и диагностика).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostMemoryImportStats {
    /// Сколько импортов создано (по одному на GPU-копию кадра).
    pub imports_created: u64,
    /// Сколько импортов сейчас живо: GPU-копии, которые ещё не выполнились.
    pub live_imports: usize,
    /// Сколько импортов не удалось (после первого импорт выключается).
    pub import_failures: u64,
}

/// Vulkan-функции и свойства устройства, общие для импортёра и его импортов.
struct HostImportVulkan {
    /// Raw device wgpu: импорты создаются и уничтожаются на нём.
    raw_device: ash::Device,
    /// `vkGetMemoryHostPointerPropertiesEXT`.
    host_pointer_functions: ash::ext::external_memory_host::Device,
    /// Memory types physical device-а (для выбора типа импорта).
    memory_types: Vec<vk::MemoryType>,
    /// Сколько импортов живо сейчас (растёт при создании, падает в Drop).
    live_imports: AtomicUsize,
}

/// Один импорт блока: `VkBuffer` поверх страниц блока на время одной GPU-копии.
struct ImportedHostBuffer {
    /// Устройство, на котором созданы buffer и memory.
    vulkan: Arc<HostImportVulkan>,
    vk_buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    /// Длина блока = размер buffer-а.
    len: u64,
    /// Держит страницы блока, пока жив импорт. Отпускается после `free_memory` (Drop ниже,
    /// затем поля): Vulkan не должен ссылаться на освобождённую память.
    _block: Arc<dyn HostImportableMemoryBlock>,
}

impl Drop for ImportedHostBuffer {
    fn drop(&mut self) {
        // SAFETY: buffer и memory созданы на `raw_device` этим импортом и больше нигде не
        // уничтожаются. Импорт отпускается drop-callback-ом обёртки, который wgpu вызывает
        // изнутри устройства после выполнения последнего submit-а с этой обёрткой (или при
        // ошибке записи до submit-а — тогда GPU его не читал). Устройство в этот момент живо.
        unsafe {
            self.vulkan.raw_device.destroy_buffer(self.vk_buffer, None);
            self.vulkan.raw_device.free_memory(self.memory, None);
        }
        self.vulkan.live_imports.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Импортёр блоков памяти кадров для одного wgpu device.
pub(super) struct HostMemoryImporter {
    /// Vulkan-функции устройства.
    vulkan: Arc<HostImportVulkan>,
    /// Счётчики (кроме `live_imports`, его считает `HostImportVulkan`).
    stats: HostMemoryImportStats,
    /// Импорт однажды не удался: дальше кадры идут через staging.
    disabled_after_failure: bool,
    /// Устройство, на котором создаются обёртки.
    device: wgpu::Device,
}

impl HostMemoryImporter {
    /// Импортёр для `device`, если устройство Vulkan и `VK_EXT_external_memory_host` включён.
    ///
    /// `None` — путь без CPU-копии недоступен, причина в логе; кадры идут через staging.
    pub(super) fn new(device: &wgpu::Device) -> Option<Self> {
        // SAFETY: guard `as_hal` живёт до конца функции; raw device/instance используются
        // только для запросов свойств и загрузки функций, ничего не уничтожается.
        let hal_device = unsafe { device.as_hal::<wgpu::hal::vulkan::Api>() }?;
        let extension_enabled = hal_device
            .enabled_device_extensions()
            .contains(&host_memory_import_extension_name());
        if !extension_enabled {
            tracing::info!(
                "Vulkan device has no VK_EXT_external_memory_host; software video frames are copied through staging"
            );
            return None;
        }
        let raw_instance = hal_device.shared_instance().raw_instance();
        let physical_device = hal_device.raw_physical_device();

        let mut host_properties = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
        let mut properties =
            vk::PhysicalDeviceProperties2::default().push_next(&mut host_properties);
        // SAFETY: physical device принадлежит instance-у этого устройства; pNext — валидная
        // структура расширения, которое включено на устройстве.
        unsafe { raw_instance.get_physical_device_properties2(physical_device, &mut properties) };
        let min_alignment = host_properties.min_imported_host_pointer_alignment;
        if !page_aligned_blocks_satisfy(min_alignment) {
            tracing::info!(
                min_imported_host_pointer_alignment = min_alignment,
                "Vulkan host pointer import needs more than page alignment; software video frames are copied through staging"
            );
            return None;
        }

        // SAFETY: как выше — запрос свойств physical device этого instance-а.
        let memory_properties =
            unsafe { raw_instance.get_physical_device_memory_properties(physical_device) };
        let memory_types = memory_properties.memory_types_as_slice().to_vec();
        let raw_device = hal_device.raw_device().clone();
        let host_pointer_functions =
            ash::ext::external_memory_host::Device::new(raw_instance, &raw_device);
        tracing::info!(
            min_imported_host_pointer_alignment = min_alignment,
            "Software video frames are uploaded from decoder memory without a CPU copy (VK_EXT_external_memory_host)"
        );
        Some(Self {
            vulkan: Arc::new(HostImportVulkan {
                raw_device,
                host_pointer_functions,
                memory_types,
                live_imports: AtomicUsize::new(0),
            }),
            stats: HostMemoryImportStats::default(),
            disabled_after_failure: false,
            device: device.clone(),
        })
    }

    /// Буфер-источник одной GPU-копии кадра: свежий импорт блока кадра.
    ///
    /// Обёртку нужно отпустить сразу после записи копий: wgpu держит её, пока submit с
    /// копией не выполнится, а потом её callback отпускает кадр и импорт. `None` — импорт
    /// недоступен, кадр нужно загрузить через staging.
    pub(super) fn frame_source_buffer(
        &mut self,
        upload: &DirectHostPlanarUpload,
    ) -> Option<wgpu::Buffer> {
        if self.disabled_after_failure {
            return None;
        }
        let imported = match import_host_block(&self.vulkan, upload.memory().block()) {
            Ok(imported) => imported,
            Err(error) => {
                self.stats.import_failures += 1;
                self.disabled_after_failure = true;
                tracing::warn!(
                    error = %error,
                    "Could not import a software video frame into Vulkan; software frames are copied through staging from now on"
                );
                return None;
            }
        };
        self.stats.imports_created += 1;
        let buffer_len = imported.len;
        let vk_buffer = imported.vk_buffer;

        // Callback держит импорт (страницы) и дескриптор кадра (содержимое) до момента,
        // когда wgpu уничтожит обёртку — после выполнения последнего submit-а с ней.
        let keepalive = (imported, upload.frame().clone());
        // SAFETY: `vk_buffer` создан на устройстве этого wgpu device и живёт, пока жив
        // `ImportedHostBuffer`, а он лежит в callback-е обёртки — значит переживает её.
        // wgpu-hal не уничтожает externally owned buffer сам и не маппит его.
        let hal_buffer = unsafe {
            wgpu::hal::vulkan::Buffer::from_raw_externally_owned(
                vk_buffer,
                Box::new(move || drop(keepalive)),
            )
        };
        // SAFETY: hal buffer создан из internal handle этого device, размер ненулевой и
        // совпадает с длиной импорта, usage `TRANSFER_SRC` соответствует `COPY_SRC`;
        // содержимое инициализировано декодером (wgpu-core считает buffer инициализированным).
        let buffer = unsafe {
            self.device
                .create_buffer_from_hal::<wgpu::hal::vulkan::Api>(
                    hal_buffer,
                    &wgpu::BufferDescriptor {
                        label: Some("host-planar-imported-frame"),
                        size: buffer_len,
                        usage: wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: false,
                    },
                )
        };
        Some(buffer)
    }

    /// Счётчики импортёра.
    pub(super) fn stats(&self) -> HostMemoryImportStats {
        HostMemoryImportStats {
            live_imports: self.vulkan.live_imports.load(Ordering::Relaxed),
            ..self.stats
        }
    }
}

/// `true`, если блоки, выровненные по странице, удовлетворяют требованию драйвера.
fn page_aligned_blocks_satisfy(min_imported_host_pointer_alignment: vk::DeviceSize) -> bool {
    min_imported_host_pointer_alignment > 0
        && (HOST_IMPORTABLE_BLOCK_ALIGNMENT as vk::DeviceSize)
            .is_multiple_of(min_imported_host_pointer_alignment)
}

/// Memory type импорта: разрешён и драйвером для этого указателя, и buffer-ом, и
/// `HOST_COHERENT`.
///
/// Coherent обязателен: память записывает декодер через свой указатель, а не через
/// `vkMapMemory`, поэтому сделать `vkFlushMappedMemoryRanges` для неё мы не можем.
fn select_host_import_memory_type(
    allowed_type_bits: u32,
    memory_types: &[vk::MemoryType],
) -> Option<u32> {
    let required = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            let bit = 1_u32.checked_shl(*index as u32).unwrap_or(0);
            allowed_type_bits & bit != 0
                && memory_type.property_flags.contains(required)
                && !memory_type
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::PROTECTED)
        })
        .and_then(|(index, _)| u32::try_from(index).ok())
}

/// Импортирует блок: `VkBuffer` + `VkDeviceMemory` поверх страниц блока.
fn import_host_block(
    vulkan: &Arc<HostImportVulkan>,
    block: &Arc<dyn HostImportableMemoryBlock>,
) -> Result<ImportedHostBuffer> {
    let region = block.region();
    let len = u64::try_from(region.len()).context("host block length does not fit u64")?;
    let host_pointer = region.address().as_ptr().cast::<c_void>();
    let raw_device = &vulkan.raw_device;

    let mut pointer_properties = vk::MemoryHostPointerPropertiesEXT::default();
    // SAFETY: функция расширения, включённого на устройстве; указатель — начало блока,
    // который жив (держим `Arc`) и выровнен по странице (контракт блока).
    unsafe {
        (vulkan
            .host_pointer_functions
            .fp()
            .get_memory_host_pointer_properties_ext)(
            raw_device.handle(),
            HOST_ALLOCATION,
            host_pointer.cast_const(),
            &mut pointer_properties,
        )
    }
    .result()
    .context("vkGetMemoryHostPointerPropertiesEXT")?;

    let mut external_buffer_info =
        vk::ExternalMemoryBufferCreateInfo::default().handle_types(HOST_ALLOCATION);
    let buffer_info = vk::BufferCreateInfo::default()
        .size(len)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info);
    // SAFETY: валидный create info с внешней памятью типа host allocation.
    let vk_buffer = unsafe { raw_device.create_buffer(&buffer_info, None) }
        .context("vkCreateBuffer for host memory import")?;

    match allocate_and_bind_host_memory(vulkan, vk_buffer, len, host_pointer, &pointer_properties) {
        Ok(memory) => {
            vulkan.live_imports.fetch_add(1, Ordering::Relaxed);
            Ok(ImportedHostBuffer {
                vulkan: Arc::clone(vulkan),
                vk_buffer,
                memory,
                len,
                _block: Arc::clone(block),
            })
        }
        Err(error) => {
            // SAFETY: buffer создан выше, ни к чему не привязан и никуда не отдан.
            unsafe { raw_device.destroy_buffer(vk_buffer, None) };
            Err(error)
        }
    }
}

/// Выделяет импортированную память под buffer и привязывает её.
fn allocate_and_bind_host_memory(
    vulkan: &HostImportVulkan,
    vk_buffer: vk::Buffer,
    len: u64,
    host_pointer: *mut c_void,
    pointer_properties: &vk::MemoryHostPointerPropertiesEXT<'_>,
) -> Result<vk::DeviceMemory> {
    let raw_device = &vulkan.raw_device;
    // SAFETY: buffer создан на этом устройстве.
    let requirements = unsafe { raw_device.get_buffer_memory_requirements(vk_buffer) };
    ensure!(
        requirements.size <= len,
        "host memory import buffer needs {} bytes, block has {len}",
        requirements.size
    );
    let memory_type_index = select_host_import_memory_type(
        requirements.memory_type_bits & pointer_properties.memory_type_bits,
        &vulkan.memory_types,
    )
    .context("no host-coherent memory type accepts this host pointer")?;

    let mut import_info = vk::ImportMemoryHostPointerInfoEXT::default()
        .handle_type(HOST_ALLOCATION)
        .host_pointer(host_pointer);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(len)
        .memory_type_index(memory_type_index)
        .push_next(&mut import_info);
    // SAFETY: указатель и длина кратны странице (контракт блока, требование драйвера
    // проверено в `HostMemoryImporter::new`); memory type разрешён для этого указателя.
    let memory = unsafe { raw_device.allocate_memory(&allocate_info, None) }
        .context("vkAllocateMemory with imported host pointer")?;
    // SAFETY: memory подходит buffer-у по типу и размеру, offset 0.
    if let Err(error) = unsafe { raw_device.bind_buffer_memory(vk_buffer, memory, 0) } {
        // SAFETY: memory выделена выше и ни к чему не привязана.
        unsafe { raw_device.free_memory(memory, None) };
        return Err(error).context("vkBindBufferMemory for imported host pointer");
    }
    Ok(memory)
}

#[cfg(test)]
mod tests;
