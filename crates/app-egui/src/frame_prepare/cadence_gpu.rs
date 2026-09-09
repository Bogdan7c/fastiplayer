//! Offscreen WGPU для проверки пикселей PreparedVideoFrame; только cfg(test).

use crate::frame_prepare::PreparedVideoFrame;
use render_core::RenderViewport;
use render_wgpu_video::{WgpuVideoRenderInput, WgpuVideoRenderer};
use std::sync::mpsc;
use std::time::Duration;

const ACCEPTANCE_TIMEOUT: Duration = Duration::from_secs(10);
const TARGET_WIDTH: u32 = 64;
const TARGET_HEIGHT: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

/// Маленький GPU consumer, независимый от окна и частоты refresh.
pub(super) struct CadenceGpu {
    /// Device владеет render/upload/readback resources.
    device: wgpu::Device,
    /// Queue исполняет draw и readback; fake release sink в этом тесте её не использует.
    queue: wgpu::Queue,
    /// Настоящий WGPU video renderer.
    renderer: WgpuVideoRenderer,
    /// Offscreen render attachment.
    target_texture: wgpu::Texture,
    /// View offscreen attachment-а.
    target_view: wgpu::TextureView,
    /// Buffer доказывает выполненный draw, а не clear-only path.
    readback_buffer: wgpu::Buffer,
    /// WGPU-required aligned copy stride.
    padded_bytes_per_row: u32,
}

impl CadenceGpu {
    /// Создаёт Vulkan device/queue; lavapipe подходит как hermetic software adapter.
    pub(super) fn new() -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("получить Vulkan adapter для frame cadence test");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("frame cadence offscreen device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("создать WGPU device для frame cadence test");
        let target_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frame cadence target"),
            size: wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let visible_bytes_per_row = TARGET_WIDTH * 4;
        let padded_bytes_per_row = visible_bytes_per_row
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame cadence readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(TARGET_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut renderer = WgpuVideoRenderer::new(&device, TARGET_FORMAT);
        renderer.resize(TARGET_WIDTH, TARGET_HEIGHT);

        Self {
            device,
            queue,
            renderer,
            target_texture,
            target_view,
            readback_buffer,
            padded_bytes_per_row,
        }
    }

    /// Даёт тесту device для настоящего materializer-а.
    pub(super) const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Даёт тесту queue для загрузки двух HostPlanar ресурсов.
    pub(super) const fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Возвращает пиксели только после завершения GPU submission; lease удерживает caller.
    pub(super) fn draw(&mut self, prepared: &PreparedVideoFrame) -> Vec<u8> {
        let renderable_frame = prepared.render_input_video_frame().expect("render input");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame cadence encoder"),
            });
        let drew_video = self
            .renderer
            .render_or_clear(WgpuVideoRenderInput {
                frame: renderable_frame.as_ref(),
                video_viewport: RenderViewport {
                    x: 0,
                    y: 0,
                    width: TARGET_WIDTH,
                    height: TARGET_HEIGHT,
                },
                video_exclusion_rects: &[],
                target: &self.target_view,
                encoder: &mut encoder,
                device: &self.device,
                queue: &self.queue,
            })
            .expect("WGPU renderer должен принять cadence frame");
        assert!(
            drew_video,
            "cadence frame не должен стать clear-only pass-ом"
        );
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(TARGET_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission_index = self.queue.submit([encoder.finish()]);
        prepared.mark_submitted_to_renderer();
        let readback_slice = self.readback_buffer.slice(..);
        let (mapping_sender, mapping_receiver) = mpsc::sync_channel(1);
        readback_slice.map_async(wgpu::MapMode::Read, move |mapping_result| {
            mapping_sender
                .send(mapping_result)
                .expect("передать cadence readback result");
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission_index),
                timeout: Some(ACCEPTANCE_TIMEOUT),
            })
            .expect("дождаться cadence WGPU submit");
        mapping_receiver
            .recv_timeout(ACCEPTANCE_TIMEOUT)
            .expect("получить cadence map callback")
            .expect("map cadence readback buffer");
        let mapped_bytes = readback_slice.get_mapped_range();
        let pixels = mapped_bytes.to_vec();
        drop(mapped_bytes);
        self.readback_buffer.unmap();
        pixels
    }
}
