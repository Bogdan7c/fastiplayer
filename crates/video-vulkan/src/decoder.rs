use anyhow::Result;
use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
use media_core::Packet;
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::{info, warn};
use video_core::{
    DecodedFrame, DecodedPixelFormat, FrameMemoryPath, FrameTextureHandle, VideoDecoder,
};

use crate::decode::{CompletedFrame, DecodeEngine, DecodeSubmission};
use crate::device::VulkanVideoDevice;
use crate::dpb::DecodedPictureBuffer;
use crate::instance::UnifiedVulkanInstance;
use crate::session::VideoSession;
use crate::vp9::build_vp9_picture_info;

/// Vulkan video decoder с реальным VP9 hardware decode.
///
/// Stage 3B: интегрирует полный decode pipeline:
/// - VideoSession для VkVideoSessionKHR
/// - DecodedPictureBuffer для 8 NV12 слотов
/// - DecodeEngine для async ring-buffered command submission
///
/// Структура stateful: управляет сменой разрешения и очередью кадров.
pub struct VulkanVideoDecoder {
    /// Vulkan устройство с video decode capability.
    vulkan_device: VulkanVideoDevice,

    /// ash Instance для создания объектов и extension loaders.
    ash_instance: ash::Instance,

    /// GPU аллокатор для выделения памяти DPB изображений.
    allocator: Arc<std::sync::Mutex<gpu_allocator::vulkan::Allocator>>,

    /// Кэш wgpu текстур для DPB слотов (one-copy interop).
    frame_texture_cache: Option<crate::frame_texture::FrameTextureCache>,

    /// Видео-сессия (создаётся при первом decode и при смене разрешения).
    session: Option<VideoSession>,

    /// Decoded Picture Buffer (создаётся вместе с сессией).
    dpb: Option<DecodedPictureBuffer>,

    /// Движок async decode (создаётся вместе с сессией).
    decode_engine: Option<DecodeEngine>,

    /// Ожидающие завершения кадры от async decode.
    pending_frames: VecDeque<CompletedFrame>,

    /// Текущее разрешение для отслеживания смены.
    current_width: u32,

    /// Текущее разрешение для отслеживания смены.
    current_height: u32,

    /// Имя бэкенда для отображения в UI.
    backend_name: &'static str,
}

impl VulkanVideoDecoder {
    /// Создаёт decoder с реальной инициализацией Vulkan video device.
    ///
    /// `unified` нужен для доступа к ash::Instance.
    ///
    /// # Safety
    /// Caller должен передать `wgpu_device`, созданный из того же Vulkan instance,
    /// который хранит `unified`. Device должен жить дольше decoder-а, потому что
    /// decoder извлекает и использует raw Vulkan handles через wgpu HAL.
    pub unsafe fn new(
        unified: &UnifiedVulkanInstance,
        wgpu_device: &wgpu::Device,
        _wgpu_queue: &wgpu::Queue,
    ) -> Result<Self> {
        let vulkan_device = unsafe { VulkanVideoDevice::from_wgpu_device(unified, wgpu_device) }?;

        let has_vp9 = vulkan_device.has_vp9_decode_support();
        let backend_name = if has_vp9 {
            "Vulkan VP9 (hw decode)"
        } else {
            "Vulkan VP9 (unavailable)"
        };

        // Получаем ash::Instance и physical_device для создания allocator.
        let (ash_instance, physical_device) = unified
            .with_ash_instance(|inst| (inst.clone(), vulkan_device.physical_device))
            .unwrap_or_else(|| {
                // Fallback: создаём минимальный instance (не должно произойти для Vulkan backend).
                let entry = unsafe { ash::Entry::load().expect("Failed to load Vulkan entry") };
                let instance = unsafe {
                    entry
                        .create_instance(&ash::vk::InstanceCreateInfo::default(), None)
                        .unwrap()
                };
                (instance, vulkan_device.physical_device)
            });

        // Создаём gpu-allocator для управления памятью DPB.
        let allocator =
            gpu_allocator::vulkan::Allocator::new(&gpu_allocator::vulkan::AllocatorCreateDesc {
                instance: ash_instance.clone(),
                device: vulkan_device.ash_device.clone(),
                physical_device,
                debug_settings: Default::default(),
                buffer_device_address: false,
                allocation_sizes: Default::default(),
            })?;
        let allocator = Arc::new(std::sync::Mutex::new(allocator));

        info!(backend_name, "VulkanVideoDecoder created");

        Ok(Self {
            vulkan_device,
            ash_instance,
            allocator,
            session: None,
            dpb: None,
            decode_engine: None,
            pending_frames: VecDeque::new(),
            current_width: 0,
            current_height: 0,
            backend_name,
            frame_texture_cache: None,
        })
    }

    /// Гарантирует, что сессия создана для текущего разрешения.
    ///
    /// Если разрешение изменилось — уничтожает старую сессию, DPB и engine,
    /// и создаёт новые.
    fn ensure_session(&mut self, width: u32, height: u32, profile: u8) -> Result<()> {
        if self.session.is_none() || width != self.current_width || height != self.current_height {
            // Сначала дропаем engine (использует device, но не allocator).
            self.decode_engine = None;

            // Затем дропаем DPB (использует allocator).
            self.dpb = None;

            // Очищаем кэш wgpu текстур — DPB слоты пересоздаются.
            self.frame_texture_cache = None;

            // Затем дропаем session.
            self.session = None;

            // Создаём новую сессию.
            let session = unsafe {
                VideoSession::new(
                    &self.ash_instance,
                    &self.vulkan_device,
                    width,
                    height,
                    profile,
                )?
            };

            // Создаём DPB с 8 слотами.
            let dpb = unsafe {
                let mut allocator = self.allocator.lock().unwrap();
                DecodedPictureBuffer::new(
                    &self.vulkan_device.ash_device,
                    &mut allocator,
                    session.picture_format,
                    width,
                    height,
                    8,
                )?
            };

            // Создаём DecodeEngine.
            let engine = unsafe {
                DecodeEngine::new(
                    &self.ash_instance,
                    self.vulkan_device.physical_device,
                    &self.vulkan_device.ash_device,
                    self.vulkan_device.video_decode_queue.unwrap(),
                    self.vulkan_device.video_decode_queue_family.unwrap(),
                    16 * 1024 * 1024, // 16MB bitstream buffer
                )?
            };

            self.session = Some(session);
            self.dpb = Some(dpb);
            self.decode_engine = Some(engine);
            self.current_width = width;
            self.current_height = height;

            info!(width, height, "Reinitialized decode session and DPB");
        }
        Ok(())
    }

    /// Get or create wgpu texture views for a DPB slot.
    ///
    /// # Safety
    /// Must be called after decode fence has signaled for this slot.
    pub unsafe fn get_or_create_wgpu_texture(
        &mut self,
        slot_index: usize,
        wgpu_device: &wgpu::Device,
    ) -> anyhow::Result<Option<(wgpu::TextureView, wgpu::TextureView)>> {
        let dpb = match self.dpb {
            Some(ref dpb) => dpb,
            None => return Ok(None),
        };

        let vk_image = match dpb.get_slot_vk_image(slot_index) {
            Some(img) => img,
            None => return Ok(None),
        };

        let slot = dpb
            .get_slot(slot_index)
            .ok_or_else(|| anyhow::anyhow!("Invalid DPB slot"))?;

        // Initialize cache if needed.
        if self.frame_texture_cache.is_none() {
            self.frame_texture_cache = Some(crate::frame_texture::FrameTextureCache::new(
                self.vulkan_device.ash_device.clone(),
                wgpu_device.clone(),
                self.allocator.clone(),
                self.vulkan_device
                    .video_decode_queue_family
                    .unwrap_or(self.vulkan_device.graphics_queue_family),
                self.vulkan_device.graphics_queue_family,
            ));
        }

        let cache = self.frame_texture_cache.as_mut().unwrap();
        unsafe { cache.get_or_create(slot_index, vk_image, slot.width, slot.height) }
    }
}

impl VideoDecoder for VulkanVideoDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>> {
        // Сначала собираем завершённые async кадры.
        if let Some(ref mut engine) = self.decode_engine {
            for completed in engine.collect_completed_frames() {
                self.pending_frames.push_back(completed);
            }
        }

        // Если есть ожидающие кадры — возвращаем самый старый.
        if let Some(completed) = self.pending_frames.pop_front() {
            return Ok(Some(DecodedFrame {
                pts: completed.pts,
                format: DecodedPixelFormat::Nv12,
                bit_depth: BitDepth::Eight,
                chroma: ChromaSubsampling::Yuv420,
                memory_path: FrameMemoryPath::DmaBufZeroCopy,
                width: completed.width,
                height: completed.height,
                render_width: completed.render_width,
                render_height: completed.render_height,
                color: VideoColorMetadata::sdr_bt709_limited(),
                texture_handle: FrameTextureHandle(completed.slot_index as u64),
            }));
        }

        // Парсим VP9 заголовок.
        let frame_info = match vp9_parser::parse_uncompressed_header(&packet.data) {
            Ok(info) => info,
            Err(e) => {
                warn!(error = %e, "VP9 parse error, skipping packet");
                return Ok(None);
            }
        };

        // Обрабатываем show_existing_frame.
        if frame_info.show_existing_frame {
            if let Some(ref dpb) = self.dpb {
                if let Some(slot) = dpb.get_existing_frame(frame_info.ref_frame_idx[0]) {
                    return Ok(Some(DecodedFrame {
                        pts: packet.pts,
                        format: DecodedPixelFormat::Nv12,
                        bit_depth: BitDepth::Eight,
                        chroma: ChromaSubsampling::Yuv420,
                        memory_path: FrameMemoryPath::DmaBufZeroCopy,
                        width: slot.width,
                        height: slot.height,
                        render_width: slot.width,
                        render_height: slot.height,
                        color: VideoColorMetadata::sdr_bt709_limited(),
                        texture_handle: FrameTextureHandle(slot.slot_index as u64),
                    }));
                }
            }
            warn!("show_existing_frame requested but DPB slot empty");
            return Ok(None);
        }

        // Пропускаем невидимые кадры (только для reference).
        if !frame_info.show_frame && !frame_info.show_existing_frame {
            return Ok(None);
        }

        if frame_info.width == 0 || frame_info.height == 0 {
            warn!("VP9 frame has zero size, skipping");
            return Ok(None);
        }

        // Гарантируем сессию для текущего разрешения.
        self.ensure_session(frame_info.width, frame_info.height, frame_info.profile)?;

        let session = self.session.as_ref().unwrap();
        let dpb = self.dpb.as_mut().unwrap();
        let engine = self.decode_engine.as_mut().unwrap();

        // Находим свободный DPB слот для декодирования.
        let dst_slot_idx = dpb
            .find_slot_for_decode(frame_info.refresh_frame_flags)
            .ok_or_else(|| anyhow::anyhow!("No free DPB slot"))?;

        // Строим VP9 picture info.
        let mut vp9_data = build_vp9_picture_info(&frame_info, dpb, dst_slot_idx as i32);

        // Устанавливаем указатель на std_picture_info.
        vp9_data.vp9_picture_info.p_std_picture_info = &vp9_data.std_picture_info;

        // Создаём decode info прямо здесь — все временные объекты живут в этом scope.
        let dst_slot = dpb
            .get_slot(dst_slot_idx)
            .expect("Invalid DPB destination slot");

        let dst_picture_resource = ash::vk::VideoPictureResourceInfoKHR::default()
            .coded_extent(ash::vk::Extent2D {
                width: frame_info.width,
                height: frame_info.height,
            })
            .image_view_binding(dst_slot.image_view);

        let setup_reference_slot = ash::vk::VideoReferenceSlotInfoKHR::default()
            .slot_index(dst_slot_idx as i32)
            .picture_resource(&dst_picture_resource);

        // Собираем опорные кадры.
        let mut reference_picture_resources = Vec::new();
        let mut reference_slots = Vec::new();

        for i in 0..8 {
            if vp9_data.ref_name_slot_indices[i] >= 0 {
                if let Some(ref_slot) = dpb.get_slot(vp9_data.ref_name_slot_indices[i] as usize) {
                    let pic_res = ash::vk::VideoPictureResourceInfoKHR::default()
                        .coded_extent(ash::vk::Extent2D {
                            width: ref_slot.width,
                            height: ref_slot.height,
                        })
                        .image_view_binding(ref_slot.image_view);

                    reference_picture_resources.push(pic_res);
                }
            }
        }

        let mut slot_idx = 0;
        for i in 0..8 {
            if vp9_data.ref_name_slot_indices[i] >= 0 {
                if let Some(_ref_slot) = dpb.get_slot(vp9_data.ref_name_slot_indices[i] as usize) {
                    reference_slots.push(
                        ash::vk::VideoReferenceSlotInfoKHR::default()
                            .slot_index(vp9_data.ref_name_slot_indices[i] as i32)
                            .picture_resource(&reference_picture_resources[slot_idx]),
                    );
                    slot_idx += 1;
                }
            }
        }

        let mut decode_info = ash::vk::VideoDecodeInfoKHR::default()
            .src_buffer(engine.bitstream_buffer())
            .src_buffer_offset(0)
            .src_buffer_range(packet.data.len() as u64)
            .dst_picture_resource(dst_picture_resource)
            .setup_reference_slot(&setup_reference_slot);

        if !reference_slots.is_empty() {
            decode_info = decode_info.reference_slots(&reference_slots);
        }

        // Submit async decode.
        unsafe {
            engine.submit_decode(DecodeSubmission {
                session,
                packet_data: &packet.data,
                decode_info: &decode_info,
                vp9_picture_info: &vp9_data.vp9_picture_info,
                completed_frame: CompletedFrame {
                    pts: packet.pts,
                    slot_index: dst_slot_idx,
                    width: frame_info.width,
                    height: frame_info.height,
                    render_width: frame_info.render_width,
                    render_height: frame_info.render_height,
                },
            })?;
        }

        // Обновляем DPB state.
        dpb.update_after_decode(
            frame_info.refresh_frame_flags,
            dst_slot_idx,
            frame_info.keyframe,
        );

        // Инвалидируем кэш wgpu текстур для обновлённых слотов.
        if let Some(ref mut cache) = self.frame_texture_cache {
            cache.invalidate_slot(dst_slot_idx);
            for i in 0..8 {
                if (frame_info.refresh_frame_flags >> i) & 1 == 1 {
                    cache.invalidate_slot(i);
                }
            }
        }

        // Возвращаем None — кадр придёт позже через collect_completed_frames.
        Ok(None)
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(ref mut engine) = self.decode_engine {
            unsafe { engine.wait_all()? };
        }
        self.pending_frames.clear();
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
