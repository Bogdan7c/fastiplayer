use std::collections::HashMap;
use std::sync::Arc;

use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, Allocator};
use tracing::debug;

/// Single wgpu-compatible frame texture (NV12 format).
pub struct WgpuFrameTexture {
    /// Underlying wgpu texture (NV12 format).
    pub texture: wgpu::Texture,
    /// Raw VkImage handle (for cleanup tracking).
    pub vk_image: vk::Image,
    /// GPU allocation for the VkImage.
    pub allocation: Allocation,
}

/// Cache of wgpu textures per DPB slot index.
pub struct FrameTextureCache {
    device: ash::Device,
    wgpu_device: wgpu::Device,
    allocator: Arc<std::sync::Mutex<Allocator>>,
    video_decode_queue_family: u32,
    graphics_queue_family: u32,
    graphics_queue: vk::Queue,
    cached: HashMap<usize, WgpuFrameTexture>,
}

impl FrameTextureCache {
    pub fn new(
        device: ash::Device,
        wgpu_device: wgpu::Device,
        allocator: Arc<std::sync::Mutex<Allocator>>,
        video_decode_queue_family: u32,
        graphics_queue_family: u32,
    ) -> Self {
        let graphics_queue = unsafe { device.get_device_queue(graphics_queue_family, 0) };
        Self {
            device,
            wgpu_device,
            allocator,
            video_decode_queue_family,
            graphics_queue_family,
            graphics_queue,
            cached: HashMap::new(),
        }
    }

    /// Get or create wgpu texture views for a DPB slot.
    ///
    /// Performs one-copy from DPB VkImage to wgpu-compatible VkImage if not cached.
    pub unsafe fn get_or_create(
        &mut self,
        slot_index: usize,
        dpb_image: vk::Image,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Option<(wgpu::TextureView, wgpu::TextureView)>> {
        // Check cache first — create views on the fly from cached texture.
        if let Some(cached) = self.cached.get(&slot_index) {
            let y_view = cached.texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("nv12 y plane"),
                format: Some(wgpu::TextureFormat::R8Unorm),
                aspect: wgpu::TextureAspect::Plane0,
                ..Default::default()
            });
            let uv_view = cached.texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("nv12 uv plane"),
                format: Some(wgpu::TextureFormat::Rg8Unorm),
                aspect: wgpu::TextureAspect::Plane1,
                ..Default::default()
            });
            return Ok(Some((y_view, uv_view)));
        }

        debug!(
            slot = slot_index,
            width, height, "Creating wgpu texture for DPB slot"
        );

        let copy_extent = vk::Extent3D {
            width,
            height,
            depth: 1,
        };

        // Queue families for CONCURRENT sharing.
        let queue_families = [self.video_decode_queue_family, self.graphics_queue_family];

        // 1. Create new VkImage with MUTABLE_FORMAT + SAMPLED + TRANSFER_DST.
        let create_info = vk::ImageCreateInfo::default()
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(copy_extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            )
            .sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(&queue_families)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let vk_image = unsafe { self.device.create_image(&create_info, None)? };

        let requirements = unsafe { self.device.get_image_memory_requirements(vk_image) };

        let allocation = {
            let mut allocator = self.allocator.lock().unwrap();
            allocator.allocate(&AllocationCreateDesc {
                name: &format!("frame_texture_{}", slot_index),
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: gpu_allocator::vulkan::AllocationScheme::DedicatedImage(
                    vk_image,
                ),
            })?
        };

        unsafe {
            self.device
                .bind_image_memory(vk_image, allocation.memory(), allocation.offset())?;
        }

        // 2. Perform one-copy from DPB image to new image.
        unsafe {
            self.copy_dpb_to_frame(dpb_image, vk_image, width, height)?;
        }

        // 3. Import into wgpu via texture_from_raw.
        let hal_device = unsafe {
            self.wgpu_device
                .as_hal::<wgpu::hal::api::Vulkan>()
                .ok_or_else(|| anyhow::anyhow!("wgpu device is not Vulkan backend"))?
        };

        let hal_texture = unsafe {
            hal_device.texture_from_raw(
                vk_image,
                &wgpu::hal::TextureDescriptor {
                    label: Some("nv12 frame"),
                    usage: wgpu_types::TextureUses::RESOURCE
                        | wgpu_types::TextureUses::COPY_DST
                        | wgpu_types::TextureUses::COPY_SRC,
                    memory_flags: wgpu::hal::MemoryFlags::empty(),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    dimension: wgpu::TextureDimension::D2,
                    sample_count: 1,
                    view_formats: Vec::new(),
                    format: wgpu::TextureFormat::NV12,
                    mip_level_count: 1,
                },
                Some(Box::new(move || {
                    // Drop callback: wgpu is done with the image.
                    // Actual cleanup happens when FrameTextureCache drops.
                })),
                wgpu::hal::vulkan::TextureMemory::External,
            )
        };

        let wgpu_texture = unsafe {
            self.wgpu_device
                .create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("nv12 frame"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::NV12,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    },
                )
        };

        // 4. Create plane views are deferred to get_or_create return path.
        let frame_texture = WgpuFrameTexture {
            texture: wgpu_texture,
            vk_image,
            allocation,
        };

        self.cached.insert(slot_index, frame_texture);

        // Create and return views from the newly inserted texture.
        let y_view = self.cached.get(&slot_index).unwrap().texture.create_view(
            &wgpu::TextureViewDescriptor {
                label: Some("nv12 y plane"),
                format: Some(wgpu::TextureFormat::R8Unorm),
                aspect: wgpu::TextureAspect::Plane0,
                ..Default::default()
            },
        );
        let uv_view = self.cached.get(&slot_index).unwrap().texture.create_view(
            &wgpu::TextureViewDescriptor {
                label: Some("nv12 uv plane"),
                format: Some(wgpu::TextureFormat::Rg8Unorm),
                aspect: wgpu::TextureAspect::Plane1,
                ..Default::default()
            },
        );
        Ok(Some((y_view, uv_view)))
    }

    /// Copy decoded DPB image to wgpu-compatible target image.
    ///
    /// Records vkCmdCopyImage for both Y and UV planes with proper barriers.
    unsafe fn copy_dpb_to_frame(
        &self,
        src_image: vk::Image,
        dst_image: vk::Image,
        width: u32,
        height: u32,
    ) -> anyhow::Result<()> {
        // Create temporary command buffer from graphics queue.
        let cmd_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.graphics_queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        let cmd_pool = unsafe { self.device.create_command_pool(&cmd_pool_info, None)? };

        let cmd_alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(cmd_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd_buffers = unsafe { self.device.allocate_command_buffers(&cmd_alloc_info)? };
        let cmd = cmd_buffers[0];

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            self.device.begin_command_buffer(cmd, &begin_info)?;
        }

        // Transition dst image from UNDEFINED to TRANSFER_DST_OPTIMAL.
        let dst_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_access_mask(vk::AccessFlags::NONE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

        // Transition src image from VIDEO_DECODE_DPB_KHR to TRANSFER_SRC_OPTIMAL.
        // VK_ACCESS_VIDEO_DECODE_WRITE_BIT_KHR = 0x00800000
        // VK_PIPELINE_STAGE_VIDEO_DECODE_BIT_KHR = 0x00020000
        let src_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(src_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_access_mask(vk::AccessFlags::from_raw(0x00800000))
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ);

        unsafe {
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::from_raw(0x00020000),
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[src_barrier, dst_barrier],
            );
        }

        // Copy plane 0 (Y).
        let copy_y = vk::ImageCopy::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::PLANE_0,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_offset(vk::Offset3D::default())
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::PLANE_0,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_offset(vk::Offset3D::default())
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            });

        // Copy plane 1 (UV).
        let copy_uv = vk::ImageCopy::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::PLANE_1,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_offset(vk::Offset3D::default())
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::PLANE_1,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_offset(vk::Offset3D::default())
            .extent(vk::Extent3D {
                width: width / 2,
                height: height / 2,
                depth: 1,
            });

        unsafe {
            self.device.cmd_copy_image(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy_y, copy_uv],
            );
        }

        // Transition dst image to GENERAL (wgpu-compatible).
        let final_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ);

        unsafe {
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[final_barrier],
            );
        }

        unsafe {
            self.device.end_command_buffer(cmd)?;
        }

        // Submit and wait.
        let cmd_array = [cmd];
        let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_array);
        let fence = unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)?
        };
        unsafe {
            self.device
                .queue_submit(self.graphics_queue, &[submit_info], fence)?;
        }
        unsafe {
            self.device.wait_for_fences(&[fence], true, u64::MAX)?;
        }

        // Cleanup.
        unsafe {
            self.device.destroy_fence(fence, None);
            self.device.free_command_buffers(cmd_pool, &[cmd]);
            self.device.destroy_command_pool(cmd_pool, None);
        }

        Ok(())
    }

    /// Invalidate cache entry when DPB slot is overwritten.
    pub fn invalidate_slot(&mut self, slot_index: usize) {
        if let Some(_entry) = self.cached.remove(&slot_index) {
            debug!(slot = slot_index, "Invalidating cached wgpu texture");
            // VkImage and allocation cleanup handled by drop.
            // Note: wgpu texture may still be in use by GPU.
            // Proper implementation needs GPU synchronization before free.
        }
    }
}

impl Drop for FrameTextureCache {
    fn drop(&mut self) {
        // Очищаем кэш перед уничтожением.
        // Wgpu текстуры должны быть уничтожены до освобождения VkImage.
        self.cached.clear();
    }
}
