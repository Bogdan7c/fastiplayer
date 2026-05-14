use anyhow::Result;
use ash::vk;
use tracing::info;

use crate::device::VulkanVideoDevice;
use crate::raw_video::*;

/// Видео-сессия Vulkan для декодирования VP9.
///
/// Управляет объектом `VkVideoSessionKHR` — ядром Vulkan Video decode pipeline.
/// Сессия создаётся с конкретным профилем (VP9), форматом (NV12) и разрешением.
/// Память для сессии выделяется и привязывается через `vkBindVideoSessionMemoryKHR`.
pub struct VideoSession {
    /// Handle Vulkan Video сессии.
    pub session: vk::VideoSessionKHR,

    /// Параметры сессии (для VP9 не требуются сложные параметры, может быть null).
    pub parameters: vk::VideoSessionParametersKHR,

    /// Формат изображения, используемый для декодирования (обычно NV12).
    pub picture_format: vk::Format,

    /// Максимальное закодированное разрешение для этой сессии.
    pub max_coded_extent: vk::Extent2D,

    /// Список привязанных блоков памяти сессии (для очистки при Drop).
    session_memory: Vec<vk::DeviceMemory>,

    /// Клон handle устройства ash для вызовов Vulkan функций и очистки.
    device: ash::Device,

    /// Function pointer для уничтожения сессии (для Drop).
    destroy_session_fn: ash::vk::PFN_vkDestroyVideoSessionKHR,

    /// Function pointer для уничтожения параметров сессии (для Drop).
    destroy_parameters_fn: Option<ash::vk::PFN_vkDestroyVideoSessionParametersKHR>,
}

impl VideoSession {
    /// Создаёт новую видео-сессию для декодирования VP9.
    ///
    /// # Аргументы
    /// * `instance` — ash Instance для загрузки extension functions.
    /// * `video_device` — устройство Vulkan с поддержкой video decode.
    /// * `coded_width` — ширина закодированного кадра.
    /// * `coded_height` — высота закодированного кадра.
    /// * `vp9_profile` — профиль VP9 (0..3).
    ///
    /// # Safety
    /// `instance` должен соответствовать physical device внутри `video_device`.
    /// `video_device` обязан иметь валидный logical device с включёнными Vulkan Video
    /// extensions, а выбранные размеры/profile должны поддерживаться драйвером.
    pub unsafe fn new(
        instance: &ash::Instance,
        video_device: &VulkanVideoDevice,
        coded_width: u32,
        coded_height: u32,
        vp9_profile: u8,
    ) -> Result<Self> {
        let device = video_device.ash_device.clone();

        // Загружаем extension functions для video_queue.
        // В ash 0.38 они доступны через khr::video_queue::Device.
        let video_queue_device = ash::khr::video_queue::Device::new(instance, &device);

        // Сохраняем function pointers для использования в Drop.
        let destroy_session_fn = video_queue_device.fp().destroy_video_session_khr;
        let destroy_parameters_fn = video_queue_device.fp().destroy_video_session_parameters_khr;

        // 1. Создаём VP9-специфичное расширение профиля.
        // Эта структура передаётся в цепочке pNext базового VideoProfileInfoKHR.
        let mut vp9_profile_info = VideoDecodeVP9ProfileInfoKHR {
            s_type: VIDEO_DECODE_VP9_PROFILE_INFO_KHR,
            p_next: std::ptr::null(),
            std_profile: match vp9_profile {
                0 => StdVideoVP9Profile::Profile0,
                1 => StdVideoVP9Profile::Profile1,
                2 => StdVideoVP9Profile::Profile2,
                3 => StdVideoVP9Profile::Profile3,
                _ => StdVideoVP9Profile::Profile0,
            },
        };

        // 2. Создаём базовый профиль видео с VP9 codec operation.
        // Вручную устанавливаем p_next, т.к. ash 0.38 не предоставляет push_next
        // для пользовательских структур в VideoProfileInfoKHR.
        let mut profile_info = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(VIDEO_CODEC_OPERATION_DECODE_VP9_BIT_KHR)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8);

        // Присоединяем VP9 профиль к цепочке pNext вручную.
        // vp9_profile_info.p_next должен указывать на текущий p_next profile_info,
        // а profile_info.p_next — на vp9_profile_info.
        vp9_profile_info.p_next = profile_info.p_next;
        profile_info.p_next = &vp9_profile_info as *const _ as *const std::ffi::c_void;

        // Для простоты MVP используем фиксированный формат NV12.
        // В production нужно query video format properties.
        let picture_format = vk::Format::G8_B8R8_2PLANE_420_UNORM;

        // 3. Создаём видео-сессию.
        let extent = vk::Extent2D {
            width: coded_width,
            height: coded_height,
        };

        let queue_family_index = video_device
            .video_decode_queue_family
            .unwrap_or(video_device.graphics_queue_family);

        let create_info = vk::VideoSessionCreateInfoKHR::default()
            .queue_family_index(queue_family_index)
            .video_profile(&profile_info)
            .picture_format(picture_format)
            .max_coded_extent(extent)
            .reference_picture_format(picture_format)
            .max_dpb_slots(8)
            .max_active_reference_pictures(8);

        // Вызываем create_video_session_khr через raw function pointer.
        let mut session = vk::VideoSessionKHR::null();
        let result = unsafe {
            (video_queue_device.fp().create_video_session_khr)(
                device.handle(),
                &create_info,
                std::ptr::null(),
                &mut session,
            )
        };

        if result != vk::Result::SUCCESS {
            return Err(anyhow::anyhow!(
                "Failed to create video session: {:?}",
                result
            ));
        }

        // 4. Запрашиваем требования к памяти сессии.
        let mut mem_req_count = 0u32;
        let result = unsafe {
            (video_queue_device
                .fp()
                .get_video_session_memory_requirements_khr)(
                device.handle(),
                session,
                &mut mem_req_count,
                std::ptr::null_mut(),
            )
        };

        if result != vk::Result::SUCCESS {
            return Err(anyhow::anyhow!(
                "Failed to get video session memory requirements count: {:?}",
                result
            ));
        }

        let mut mem_reqs =
            vec![vk::VideoSessionMemoryRequirementsKHR::default(); mem_req_count as usize];
        let result = unsafe {
            (video_queue_device
                .fp()
                .get_video_session_memory_requirements_khr)(
                device.handle(),
                session,
                &mut mem_req_count,
                mem_reqs.as_mut_ptr(),
            )
        };

        if result != vk::Result::SUCCESS {
            return Err(anyhow::anyhow!(
                "Failed to get video session memory requirements: {:?}",
                result
            ));
        }

        // 5. Выделяем и привязываем память для каждого требования.
        let mut session_memory = Vec::with_capacity(mem_req_count as usize);
        let mut bind_infos = Vec::with_capacity(mem_req_count as usize);

        for (i, req) in mem_reqs.iter().enumerate() {
            // Получаем требования к памяти из структуры.
            let memory_requirements = req.memory_requirements;

            // Выделяем память через vkAllocateMemory.
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(memory_requirements.size)
                .memory_type_index(memory_requirements.memory_type_bits.trailing_zeros());

            let memory = unsafe { device.allocate_memory(&alloc_info, None)? };
            session_memory.push(memory);

            // Формируем информацию для привязки.
            bind_infos.push(
                vk::BindVideoSessionMemoryInfoKHR::default()
                    .memory_bind_index(i as u32)
                    .memory(memory)
                    .memory_offset(0)
                    .memory_size(memory_requirements.size),
            );
        }

        // Привязываем память к сессии.
        let result = unsafe {
            (video_queue_device.fp().bind_video_session_memory_khr)(
                device.handle(),
                session,
                bind_infos.len() as u32,
                bind_infos.as_ptr(),
            )
        };

        if result != vk::Result::SUCCESS {
            return Err(anyhow::anyhow!(
                "Failed to bind video session memory: {:?}",
                result
            ));
        }

        info!(
            width = coded_width,
            height = coded_height,
            profile = vp9_profile,
            "VideoSession created"
        );

        Ok(Self {
            session,
            parameters: vk::VideoSessionParametersKHR::null(),
            picture_format,
            max_coded_extent: extent,
            session_memory,
            device,
            destroy_session_fn,
            destroy_parameters_fn: Some(destroy_parameters_fn),
        })
    }
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        unsafe {
            // Сначала уничтожаем параметры сессии (если есть).
            if self.parameters != vk::VideoSessionParametersKHR::null() {
                if let Some(destroy_fn) = self.destroy_parameters_fn {
                    destroy_fn(self.device.handle(), self.parameters, std::ptr::null());
                }
            }

            // Уничтожаем саму сессию через сохранённый function pointer.
            (self.destroy_session_fn)(self.device.handle(), self.session, std::ptr::null());

            // Освобождаем память сессии.
            for &memory in &self.session_memory {
                self.device.free_memory(memory, None);
            }
        }
    }
}
