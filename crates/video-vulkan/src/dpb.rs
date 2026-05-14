use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, Allocator};
use tracing::debug;

/// Один слот Decoded Picture Buffer (DPB).
///
/// Каждый слот содержит NV12 изображение, размещённое в GPU памяти,
/// и представление изображения (image view) для использования в Vulkan Video.
/// Слоты используются для хранения декодированных опорных кадров VP9.
pub struct DpbSlot {
    /// Индекс слота в DPB (0..7).
    pub slot_index: i32,

    /// Vulkan изображение (NV12 формат).
    pub image: vk::Image,

    /// Представление изображения (view) для привязки к декодеру.
    pub image_view: vk::ImageView,

    /// GPU-аллокация памяти для изображения.
    pub allocation: Allocation,

    /// Ширина изображения в пикселях.
    pub width: u32,

    /// Высота изображения в пикселях.
    pub height: u32,

    /// Флаг: слот занят (содержит действительный декодированный кадр).
    pub in_use: bool,

    /// Флаг: кадр в слоте является ключевым (keyframe).
    pub is_keyframe: bool,
}

/// Decoded Picture Buffer — буфер декодированных кадров VP9.
///
/// VP9 использует до 8 опорных кадров, хранящихся в DPB.
/// Каждый слот — это NV12 изображение, выделенное в GPU памяти через gpu-allocator.
/// DPB управляет жизненным циклом слотов: создание, обновление, поиск, очистка.
pub struct DecodedPictureBuffer {
    /// Слоты DPB (обычно 8 штук для VP9).
    slots: Vec<DpbSlot>,

    /// Клон handle устройства Vulkan для очистки ресурсов.
    device: ash::Device,
}

impl DecodedPictureBuffer {
    /// Создаёт DPB с заданным количеством слотов.
    ///
    /// # Аргументы
    /// * `device` — Vulkan устройство.
    /// * `allocator` — GPU аллокатор для выделения памяти изображений.
    /// * `format` — формат изображения (обычно NV12).
    /// * `width` — ширина кадра.
    /// * `height` — высота кадра.
    /// * `slot_count` — количество слотов (для VP9 обычно 8).
    ///
    /// # Safety
    /// `device` и `allocator` должны принадлежать одному Vulkan logical device.
    /// Caller обязан гарантировать, что allocator живёт дольше DPB и что `format`
    /// поддерживает `VIDEO_DECODE_DPB_KHR | VIDEO_DECODE_DST_KHR | SAMPLED` usage
    /// для выбранного physical device.
    pub unsafe fn new(
        device: &ash::Device,
        allocator: &mut Allocator,
        format: vk::Format,
        width: u32,
        height: u32,
        slot_count: usize,
    ) -> anyhow::Result<Self> {
        let mut slots = Vec::with_capacity(slot_count);

        for i in 0..slot_count {
            // Создаём информацию об изображении.
            // Изображение должно поддерживать использование как:
            // - DPB (Decoded Picture Buffer) для хранения опорных кадров
            // - DST (Destination) для записи декодированного кадра
            // - SAMPLED для последующего чтения шейдерами (Stage 3C)
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(
                    vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR
                        | vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);

            // Создаём Vulkan изображение.
            let image = unsafe { device.create_image(&image_info, None)? };

            // Получаем требования к памяти для изображения.
            let requirements = unsafe { device.get_image_memory_requirements(image) };

            // Выделяем память через gpu-allocator с dedicated allocation.
            // DedicatedImage гарантирует, что изображение получит собственный
            // выделенный блок памяти (не под-аллокацию из общего пула).
            let allocation = allocator.allocate(&AllocationCreateDesc {
                name: &format!("dpb_slot_{}", i),
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false, // Изображения используют оптимальное (non-linear) расположение
                allocation_scheme: gpu_allocator::vulkan::AllocationScheme::DedicatedImage(image),
            })?;

            // Привязываем память к изображению.
            unsafe {
                device.bind_image_memory(image, allocation.memory(), allocation.offset())?;
            };

            // Создаём view изображения для использования в Vulkan Video.
            // Aspect mask COLOR, т.к. NV12 хранит яркость и цветность в одном изображении.
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let image_view = unsafe { device.create_image_view(&view_info, None)? };

            slots.push(DpbSlot {
                slot_index: i as i32,
                image,
                image_view,
                allocation,
                width,
                height,
                in_use: false,
                is_keyframe: false,
            });

            debug!(slot = i, width, height, "DPB slot created");
        }

        Ok(Self {
            slots,
            device: device.clone(),
        })
    }

    /// Находит первый свободный слот или слот, который будет обновлён.
    ///
    /// Если `refresh_flags` указывает на конкретные слоты для обновления,
    /// возвращает первый такой слот. Иначе ищет первый незанятый слот.
    pub fn find_slot_for_decode(&self, refresh_flags: u8) -> Option<usize> {
        // Если бит установлен в refresh_flags, слот будет перезаписан — можно использовать.
        for i in 0..self.slots.len() {
            if (refresh_flags >> i) & 1 == 1 || !self.slots[i].in_use {
                return Some(i);
            }
        }
        // Fallback: ищем любой незанятый слот.
        self.slots.iter().position(|s| !s.in_use)
    }

    /// Получает слот по индексу (immutable ссылка).
    pub fn get_slot(&self, index: usize) -> Option<&DpbSlot> {
        self.slots.get(index)
    }

    /// Получает слот по индексу (mutable ссылка).
    pub fn get_slot_mut(&mut self, index: usize) -> Option<&mut DpbSlot> {
        self.slots.get_mut(index)
    }

    /// Отображает VP9 ref_frame_idx в индекс слота DPB.
    ///
    /// Для MVP используется прямое отображение:
    /// ref_frame_idx[i] == индекс слота DPB.
    pub fn map_reference(&self, ref_idx: u8) -> Option<i32> {
        self.slots.get(ref_idx as usize).map(|s| s.slot_index)
    }

    /// Обновляет состояние слотов после успешного декодирования.
    ///
    /// Помечает декодированный слот как занятый и обновляет флаги
    /// для слотов, указанных в refresh_frame_flags.
    pub fn update_after_decode(
        &mut self,
        refresh_flags: u8,
        decoded_slot: usize,
        is_keyframe: bool,
    ) {
        if let Some(slot) = self.slots.get_mut(decoded_slot) {
            slot.in_use = true;
            slot.is_keyframe = is_keyframe;
        }

        // Помечаем обновлённые слоты как занятые.
        for i in 0..self.slots.len() {
            if (refresh_flags >> i) & 1 == 1 {
                if let Some(slot) = self.slots.get_mut(i) {
                    slot.in_use = true;
                }
            }
        }
    }

    /// Обрабатывает show_existing_frame: возвращает существующий слот без декодирования.
    ///
    /// Проверяет, что слот действительно занят (содержит валидный кадр).
    pub fn get_existing_frame(&self, frame_to_show_map_idx: u8) -> Option<&DpbSlot> {
        self.slots
            .get(frame_to_show_map_idx as usize)
            .filter(|s| s.in_use)
    }

    /// Returns the raw VkImage handle for a DPB slot.
    pub fn get_slot_vk_image(&self, index: usize) -> Option<vk::Image> {
        self.slots.get(index).map(|s| s.image)
    }

    /// Возвращает количество слотов в DPB.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

impl Drop for DecodedPictureBuffer {
    fn drop(&mut self) {
        // Важно: память должна быть освобождена через allocator.free()
        // ПЕРЕД уничтожением изображений. Но поскольку DecodedPictureBuffer
        // владеет Allocation объектами, а Allocator внешний — caller должен
        // гарантировать, что Allocator живёт дольше DPB.
        //
        // Порядок очистки:
        // 1. Уничтожить image_view
        // 2. Уничтожить image
        // 3. Освободить allocation (через внешний allocator)
        //
        // Примечание: allocation освобождается при Drop DpbSlot,
        // но для этого нужен mutable доступ к allocator.
        // В текущей архитектуре caller отвечает за освобождение allocation
        // после уничтожения DPB.

        unsafe {
            for slot in &self.slots {
                self.device.destroy_image_view(slot.image_view, None);
                self.device.destroy_image(slot.image, None);
            }
        }
    }
}
