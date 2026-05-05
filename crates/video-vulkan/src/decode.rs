use anyhow::Result;
use ash::vk;
use std::collections::VecDeque;
use std::time::Duration;
use tracing::trace;

use crate::session::VideoSession;

/// Завершённый декодированный кадр.
///
/// Содержит метаданные о кадре, который был успешно декодирован GPU.
/// Само изображение остаётся в DPB (GPU памяти), этот struct только
/// индексирует его для дальнейшей обработки.
pub struct CompletedFrame {
    /// Presentation timestamp кадра.
    pub pts: Duration,

    /// Индекс слота DPB, в котором хранится декодированный кадр.
    pub slot_index: usize,

    /// Ширина кадра.
    pub width: u32,

    /// Высота кадра.
    pub height: u32,

    /// Ширина области отображения (может отличаться от coded width).
    pub render_width: u32,

    /// Высота области отображения.
    pub render_height: u32,
}

/// Движок асинхронного декодирования видео с ring buffer.
///
/// Использует 4 command buffers + fences для пипелининга.
/// CPU может submit'ить новые кадры, не дожидаясь завершения предыдущих.
/// Завершённые кадры собираются отдельно через `collect_completed_frames()`.
pub struct DecodeEngine {
    /// Vulkan устройство.
    device: ash::Device,

    /// Очередь видео-декодирования.
    video_queue: vk::Queue,

    /// Пул команд для видео-очереди.
    command_pool: vk::CommandPool,

    /// Размер ring buffer (обычно 4).
    ring_size: usize,

    /// Command buffers ring.
    command_buffers: Vec<vk::CommandBuffer>,

    /// Fences для отслеживания завершения (по одному на ring slot).
    fences: Vec<vk::Fence>,

    /// Флаги: fence активен (команда в полёте).
    fence_in_flight: Vec<bool>,

    /// Текущий индекс в ring buffer.
    ring_index: usize,

    /// Очередь завершённых кадров (для возврата через collect_completed_frames).
    completed_frames: VecDeque<CompletedFrame>,

    /// Vulkan буфер для загрузки битстрима.
    bitstream_buffer: vk::Buffer,

    /// Память битстрим-буфера.
    bitstream_memory: vk::DeviceMemory,

    /// Ёмкость битстрим-буфера в байтах.
    _bitstream_capacity: usize,

    /// Function pointer для cmd_decode_video_khr.
    cmd_decode_video_fn: ash::vk::PFN_vkCmdDecodeVideoKHR,

    /// Function pointer для cmd_begin_video_coding_khr.
    cmd_begin_video_coding_fn: ash::vk::PFN_vkCmdBeginVideoCodingKHR,

    /// Function pointer для cmd_end_video_coding_khr.
    cmd_end_video_coding_fn: ash::vk::PFN_vkCmdEndVideoCodingKHR,
}

impl DecodeEngine {
    /// Возвращает handle битстрим-буфера для использования в decode info.
    pub fn bitstream_buffer(&self) -> vk::Buffer {
        self.bitstream_buffer
    }

    /// Создаёт новый DecodeEngine.
    /// Создаёт новый DecodeEngine.
    ///
    /// # Аргументы
    /// * `instance` — ash Instance для загрузки extension functions.
    /// * `device` — Vulkan устройство.
    /// * `video_queue` — очередь видео-декодирования.
    /// * `video_queue_family` — индекс семейства очередей видео.
    /// * `bitstream_capacity` — размер битстрим-буфера в байтах.
    pub unsafe fn new(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        device: &ash::Device,
        video_queue: vk::Queue,
        video_queue_family: u32,
        bitstream_capacity: usize,
    ) -> Result<Self> {
        let ring_size = 4;

        // Загружаем extension functions для video decode.
        let video_decode_device = ash::khr::video_decode_queue::Device::new(instance, device);
        let video_queue_device = ash::khr::video_queue::Device::new(instance, device);

        let cmd_decode_video_fn = video_decode_device.fp().cmd_decode_video_khr;
        let cmd_begin_video_coding_fn = video_queue_device.fp().cmd_begin_video_coding_khr;
        let cmd_end_video_coding_fn = video_queue_device.fp().cmd_end_video_coding_khr;

        // Создаём command pool для видео очереди.
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(video_queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { device.create_command_pool(&pool_info, None)? };

        // Аллоцируем command buffers.
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(ring_size as u32);
        let command_buffers = unsafe { device.allocate_command_buffers(&alloc_info)? };

        // Создаём fences (изначально в сигнализированном состоянии,
        // чтобы первый кадр мог сразу использовать slot 0).
        let mut fences = Vec::with_capacity(ring_size);
        for _ in 0..ring_size {
            let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
            fences.push(unsafe { device.create_fence(&fence_info, None)? });
        }

        // Создаём битстрим-буфер (CPU-visible для загрузки данных).
        let buffer_info = vk::BufferCreateInfo::default()
            .size(bitstream_capacity as u64)
            .usage(vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let bitstream_buffer = unsafe { device.create_buffer(&buffer_info, None)? };

        // Определяем требования к памяти и находим подходящий memory type.
        let mem_reqs = unsafe { device.get_buffer_memory_requirements(bitstream_buffer) };
        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };

        // Находим memory type с HOST_VISIBLE | HOST_COHERENT.
        let mut mem_type_index = 0;
        for i in 0..mem_props.memory_type_count {
            let props = mem_props.memory_types[i as usize].property_flags;
            if mem_reqs.memory_type_bits & (1 << i) != 0
                && props.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
            {
                mem_type_index = i;
                if props.contains(vk::MemoryPropertyFlags::HOST_COHERENT) {
                    break; // Предпочитаем coherent память
                }
            }
        }

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type_index);
        let bitstream_memory = unsafe { device.allocate_memory(&alloc_info, None)? };

        unsafe { device.bind_buffer_memory(bitstream_buffer, bitstream_memory, 0)? };

        Ok(Self {
            device: device.clone(),
            video_queue,
            command_pool,
            ring_size,
            command_buffers,
            fences,
            fence_in_flight: vec![false; ring_size],
            ring_index: 0,
            completed_frames: VecDeque::new(),
            bitstream_buffer,
            bitstream_memory,
            _bitstream_capacity: bitstream_capacity,
            cmd_decode_video_fn,
            cmd_begin_video_coding_fn,
            cmd_end_video_coding_fn,
        })
    }

    /// Асинхронно submit'ит VP9 decode команду.
    ///
    /// Возвращает немедленно. Кадр будет доступен позже через `collect_completed_frames()`.
    pub unsafe fn submit_decode(
        &mut self,
        session: &VideoSession,
        packet_data: &[u8],
        decode_info: &vk::VideoDecodeInfoKHR,
        vp9_picture_info: &crate::raw_video::VideoDecodeVP9PictureInfoKHR,
        pts: Duration,
        slot_index: usize,
        width: u32,
        height: u32,
        render_width: u32,
        render_height: u32,
    ) -> Result<()> {
        let ring_idx = self.ring_index % self.ring_size;

        // Ждём, если текущий ring slot ещё используется GPU.
        if self.fence_in_flight[ring_idx] {
            unsafe {
                self.device
                    .wait_for_fences(&[self.fences[ring_idx]], true, u64::MAX)?;
            }
            self.fence_in_flight[ring_idx] = false;
        }

        // Сбрасываем fence и command buffer.
        unsafe {
            self.device.reset_fences(&[self.fences[ring_idx]])?;
            self.device.reset_command_buffer(
                self.command_buffers[ring_idx],
                vk::CommandBufferResetFlags::empty(),
            )?;
        }

        // Копируем packet data в битстрим-буфер.
        unsafe {
            let data_ptr = self.device.map_memory(
                self.bitstream_memory,
                0,
                packet_data.len() as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(
                packet_data.as_ptr(),
                data_ptr as *mut u8,
                packet_data.len(),
            );
            self.device.unmap_memory(self.bitstream_memory);
        }

        // Записываем command buffer.
        let cmd = self.command_buffers[ring_idx];
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            self.device.begin_command_buffer(cmd, &begin_info)?;
        }

        // Начинаем video coding scope.
        let begin_coding = vk::VideoBeginCodingInfoKHR::default()
            .video_session(session.session)
            .video_session_parameters(session.parameters);
        unsafe {
            (self.cmd_begin_video_coding_fn)(cmd, &begin_coding);
        }

        // Декодируем видео (VP9 picture info в pNext).
        let mut decode_info_with_next = decode_info.clone();
        decode_info_with_next.p_next = vp9_picture_info as *const _ as *const std::ffi::c_void;
        unsafe {
            (self.cmd_decode_video_fn)(cmd, &decode_info_with_next);
        }

        // Завершаем video coding scope.
        let end_coding = vk::VideoEndCodingInfoKHR::default();
        unsafe {
            (self.cmd_end_video_coding_fn)(cmd, &end_coding);
        }

        unsafe {
            self.device.end_command_buffer(cmd)?;
        }

        // Submit на video decode queue.
        let cmd_buffers = [cmd];
        let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_buffers);
        let submit_infos = [submit_info];
        unsafe {
            self.device
                .queue_submit(self.video_queue, &submit_infos, self.fences[ring_idx])?;
        }

        self.fence_in_flight[ring_idx] = true;
        self.ring_index += 1;

        // Сохраняем информацию о кадре (будет возвращена при завершении).
        self.completed_frames.push_back(CompletedFrame {
            pts,
            slot_index,
            width,
            height,
            render_width,
            render_height,
        });

        trace!("Submitted VP9 decode to ring slot {}", ring_idx);
        Ok(())
    }

    /// Проверяет все fences и возвращает завершённые кадры.
    pub fn collect_completed_frames(&mut self) -> Vec<CompletedFrame> {
        let mut ready = Vec::new();

        for i in 0..self.ring_size {
            if self.fence_in_flight[i] {
                let signaled = unsafe {
                    self.device
                        .get_fence_status(self.fences[i])
                        .unwrap_or(false)
                };
                if signaled {
                    self.fence_in_flight[i] = false;
                    if let Some(frame) = self.completed_frames.pop_front() {
                        ready.push(frame);
                    }
                }
            }
        }

        ready
    }

    /// Блокирует выполнение до завершения всех pending decode операций.
    pub unsafe fn wait_all(&mut self) -> Result<()> {
        let active_fences: Vec<_> = (0..self.ring_size)
            .filter(|&i| self.fence_in_flight[i])
            .map(|i| self.fences[i])
            .collect();

        if !active_fences.is_empty() {
            unsafe {
                self.device
                    .wait_for_fences(&active_fences, true, u64::MAX)?
            };
            for i in 0..self.ring_size {
                self.fence_in_flight[i] = false;
            }
        }
        Ok(())
    }
}

impl Drop for DecodeEngine {
    fn drop(&mut self) {
        unsafe {
            // Ждём завершения всех операций.
            let _ = self.wait_all();

            // Уничтожаем битстрим-буфер.
            self.device.destroy_buffer(self.bitstream_buffer, None);
            self.device.free_memory(self.bitstream_memory, None);

            // Уничтожаем fences.
            for &fence in &self.fences {
                self.device.destroy_fence(fence, None);
            }

            // Освобождаем command buffers.
            self.device
                .free_command_buffers(self.command_pool, &self.command_buffers);

            // Уничтожаем command pool.
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}
