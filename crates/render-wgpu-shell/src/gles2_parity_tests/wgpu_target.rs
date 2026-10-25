//! wgpu-сторона паритета: настоящие `EguiCompositor` и `WindowCornerMaskRenderer` шелла на
//! offscreen-текстуре `Rgba8Unorm` (тот же порядок, что `record_and_present_frame`: «видео»
//! (очистка) → egui → маска → submit → `free`).

use std::sync::mpsc;
use std::time::Duration;

use render_shell_api::WindowCornerMask;

use crate::egui_compositor::{EguiCompositor, UiTextureUpload};
use crate::window_corner_mask::{SurfaceAlphaEncoding, WindowCornerMaskRenderer};

use super::scenes::ParityFrame;

pub(super) const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GPU_TIMEOUT: Duration = Duration::from_secs(10);

/// Headless wgpu-устройство паритета: тот же Vulkan-instance, что у production (сессия 04,
/// дефект 3: GL-бэкенд wgpu делил бы `EGLDisplay` с GLES2). `None` — нет адаптера.
pub(super) fn parity_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = crate::shell::gpu_context::vulkan_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    let device_and_queue = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gles2 parity wgpu device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .expect("create parity wgpu device");
    Some(device_and_queue)
}

/// Headless wgpu + цель + художник и маска шелла.
pub(super) struct WgpuParityTarget {
    device: wgpu::Device,
    queue: wgpu::Queue,
    size: [u32; 2],
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    compositor: EguiCompositor,
    corner_mask: WindowCornerMaskRenderer,
}

impl WgpuParityTarget {
    /// `None` — нет GPU-адаптера (тест пропускается с сообщением).
    pub(super) fn new(size: [u32; 2]) -> Option<Self> {
        let (device, queue) = parity_device()?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gles2 parity wgpu target"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let compositor = EguiCompositor::new(&device, TARGET_FORMAT);
        let corner_mask = WindowCornerMaskRenderer::new(
            &device,
            TARGET_FORMAT,
            SurfaceAlphaEncoding::Premultiplied,
        );
        Some(Self {
            device,
            queue,
            size,
            texture,
            view,
            compositor,
            corner_mask,
        })
    }

    /// Нарисовать кадр и вернуть пиксели RGBA8 (строки сверху вниз, без выравнивания).
    pub(super) fn render(&mut self, frame: &ParityFrame) -> Vec<u8> {
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: self.size,
            pixels_per_point: frame.pixels_per_point,
        };
        let mut encoder = self.encoder_cleared_to(frame.background);
        let retired_textures = match &frame.ui {
            Some((primitives, textures_delta)) => {
                let upload = self.compositor.upload_changed_textures(
                    &self.device,
                    &self.queue,
                    textures_delta,
                );
                assert_eq!(upload, UiTextureUpload::InSync, "wgpu textures in sync");
                let callbacks = self.compositor.prepare_fresh_frame(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    primitives.clone(),
                    screen,
                );
                assert!(
                    callbacks.is_empty(),
                    "parity scenes have no paint callbacks"
                );
                Some(textures_delta.clone())
            }
            None => {
                assert_eq!(
                    self.compositor.retained_frame_availability(&screen),
                    Ok(()),
                    "reuse needs the retained frame"
                );
                None
            }
        };
        self.compositor
            .render_retained_overlay(&mut encoder, &self.view);
        self.corner_mask.render(
            &self.queue,
            &mut encoder,
            &self.view,
            self.size,
            frame.pixels_per_point,
            WindowCornerMask::rounded_in_points(frame.corner_radius_points),
        );
        let pixels = self.read_back(encoder);
        // `free` — после submit кадра, как у шелла.
        if let Some(textures_delta) = retired_textures {
            self.compositor.free_retired_textures(textures_delta);
        }
        pixels
    }

    /// «Видео»: очистка цели цветом, как video pass без кадра.
    fn encoder_cleared_to(&self, color: [f32; 4]) -> wgpu::CommandEncoder {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gles2 parity wgpu encoder"),
            });
        let [r, g, b, a] = color.map(f64::from);
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gles2 parity wgpu background"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        encoder
    }

    fn read_back(&self, encoder: wgpu::CommandEncoder) -> Vec<u8> {
        read_back_texture(&self.device, &self.queue, &self.texture, self.size, encoder)
    }
}

/// Отправить `encoder` вместе с копией `texture` в буфер и вернуть плотные RGBA8 строки сверху
/// вниз (отступы строк копии `COPY_BYTES_PER_ROW_ALIGNMENT` срезаются).
pub(super) fn read_back_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
    encoder: wgpu::CommandEncoder,
) -> Vec<u8> {
    read_back_texture_after(device, queue, texture, size, Vec::new(), encoder)
}

/// То же, но сначала отправляются `preceding` (например, копия загрузки кадра) — одним submit-ом.
pub(super) fn read_back_texture_after(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
    preceding: Vec<wgpu::CommandBuffer>,
    encoder: wgpu::CommandEncoder,
) -> Vec<u8> {
    read_back_texture_around(device, queue, texture, size, preceding, Vec::new(), encoder)
}

/// То же, но барьеры вокруг кадра: `preceding` уходит до `encoder`, `following` — после него
/// (acquire/release внешней очереди для DMA-BUF), всё одним submit-ом, как в шелле.
pub(super) fn read_back_texture_around(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
    preceding: Vec<wgpu::CommandBuffer>,
    following: Vec<wgpu::CommandBuffer>,
    mut encoder: wgpu::CommandEncoder,
) -> Vec<u8> {
    let [width, height] = size;
    let bytes_per_row = width * 4;
    let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gles2 parity wgpu readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(
        preceding
            .into_iter()
            .chain([encoder.finish()])
            .chain(following),
    );
    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("send readback result");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_TIMEOUT),
        })
        .expect("wait for GPU");
    receiver
        .recv_timeout(GPU_TIMEOUT)
        .expect("readback callback")
        .expect("map readback");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let pixels = mapped
        .chunks_exact(padded as usize)
        .flat_map(|row| row[..bytes_per_row as usize].iter().copied())
        .collect::<Vec<_>>();
    drop(mapped);
    readback.unmap();
    pixels
}
