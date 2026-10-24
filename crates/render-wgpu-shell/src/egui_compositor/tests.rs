//! Повторное рисование удержанного egui-кадра: настоящий egui pass → tessellate → GPU → пиксели.
//!
//! Тесты пропускаются (с сообщением), если на машине нет GPU adapter-а, как и остальные
//! GPU-тесты shell-а.

use std::sync::mpsc;
use std::time::Duration;

use super::{EguiCompositor, RetainedEguiFrameUnavailable};

const TARGET_WIDTH: u32 = 256;
const TARGET_HEIGHT: u32 = 144;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GPU_TEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Прямоугольник интерфейса в points (scale 1.0): красный блок в левом верхнем углу.
const UI_RECT: egui::Rect =
    egui::Rect::from_min_max(egui::pos2(16.0, 16.0), egui::pos2(80.0, 48.0));

/// Headless GPU + целевая текстура + compositor под тестом.
struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    compositor: EguiCompositor,
}

impl Harness {
    /// `None`, если нет GPU adapter-а.
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("egui compositor test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("create egui compositor test device");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui compositor test target"),
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
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let compositor = EguiCompositor::new(&device, TARGET_FORMAT);
        Some(Self {
            device,
            queue,
            texture,
            view,
            compositor,
        })
    }

    /// Encoder с «видео»: очистка target-а сплошным цветом, как делает video pass.
    fn encoder_with_video_cleared_to(&self, color: wgpu::Color) -> wgpu::CommandEncoder {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("egui compositor test encoder"),
            });
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui compositor test video"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
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

    fn read_back(&self, mut encoder: wgpu::CommandEncoder) -> Vec<u8> {
        let bytes_per_row = TARGET_WIDTH * 4;
        let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui compositor test readback"),
            size: u64::from(padded) * u64::from(TARGET_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(TARGET_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("send readback result");
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_TEST_TIMEOUT),
            })
            .expect("wait for GPU");
        receiver
            .recv_timeout(GPU_TEST_TIMEOUT)
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
}

macro_rules! gpu_or_skip {
    () => {
        match Harness::new() {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for egui compositor test");
                return;
            }
        }
    };
}

fn screen_descriptor() -> egui_wgpu::ScreenDescriptor {
    egui_wgpu::ScreenDescriptor {
        size_in_pixels: [TARGET_WIDTH, TARGET_HEIGHT],
        pixels_per_point: 1.0,
    }
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * TARGET_WIDTH + x) * 4) as usize;
    pixels[offset..offset + 4].try_into().expect("pixel")
}

/// Настоящий egui pass: красный прямоугольник и delta шрифтовой текстуры первого кадра.
fn tessellated_ui_frame() -> (Vec<egui::epaint::ClippedPrimitive>, egui::TexturesDelta) {
    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(TARGET_WIDTH as f32, TARGET_HEIGHT as f32),
        )),
        ..Default::default()
    };
    // egui 0.36: `run_ui` отдаёт корневой `Ui` на весь экран без собственного фона.
    let full_output = ctx.run_ui(raw_input, |ui| {
        ui.painter().rect_filled(UI_RECT, 0.0, egui::Color32::RED);
    });
    let paint_jobs = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
    (paint_jobs, full_output.textures_delta)
}

/// Повторный кадр рисует те же пиксели интерфейса поверх другого «видео» без update_buffers.
#[test]
fn reused_frame_draws_identical_ui_pixels_over_new_video() {
    let mut harness = gpu_or_skip!();
    let (paint_jobs, textures_delta) = tessellated_ui_frame();
    let screen = screen_descriptor();

    // Свежий кадр: синее «видео», свежие примитивы.
    let mut encoder = harness.encoder_with_video_cleared_to(wgpu::Color::BLUE);
    let upload = harness.compositor.upload_changed_textures(
        &harness.device,
        &harness.queue,
        &textures_delta,
    );
    assert_eq!(upload, super::UiTextureUpload::InSync);
    let callback_buffers = harness.compositor.prepare_fresh_frame(
        &harness.device,
        &harness.queue,
        &mut encoder,
        paint_jobs,
        screen_descriptor(),
    );
    assert!(callback_buffers.is_empty(), "no paint callbacks in this UI");
    harness
        .compositor
        .render_retained_overlay(&mut encoder, &harness.view);
    harness.compositor.free_retired_textures(textures_delta);
    let fresh_pixels = harness.read_back(encoder);

    // Повторный кадр: зелёное «видео», интерфейс — из удержанных примитивов.
    assert_eq!(
        harness.compositor.retained_frame_availability(&screen),
        Ok(())
    );
    let mut encoder = harness.encoder_with_video_cleared_to(wgpu::Color::GREEN);
    harness
        .compositor
        .render_retained_overlay(&mut encoder, &harness.view);
    let reused_pixels = harness.read_back(encoder);

    let inside_ui = (32, 24);
    let outside_ui = (200, 100);
    assert_eq!(
        pixel(&fresh_pixels, inside_ui.0, inside_ui.1),
        [255, 0, 0, 255],
        "fresh frame must paint the red UI rect"
    );
    assert_eq!(
        pixel(&reused_pixels, inside_ui.0, inside_ui.1),
        [255, 0, 0, 255],
        "reused frame must paint the same red UI rect"
    );
    assert_eq!(
        pixel(&fresh_pixels, outside_ui.0, outside_ui.1),
        [0, 0, 255, 255],
        "fresh frame shows the blue video outside the UI"
    );
    assert_eq!(
        pixel(&reused_pixels, outside_ui.0, outside_ui.1),
        [0, 255, 0, 255],
        "reused frame shows the NEW green video outside the UI"
    );
}

/// Без свежего кадра или при другом размере экрана повторное рисование отвергается.
#[test]
fn retained_frame_is_refused_without_fresh_frame_or_with_other_screen() {
    let mut harness = gpu_or_skip!();
    let screen = screen_descriptor();

    assert_eq!(
        harness.compositor.retained_frame_availability(&screen),
        Err(RetainedEguiFrameUnavailable::NothingRetained)
    );
    // Без удержанного кадра overlay pass не открывается, а «видео» остаётся нетронутым.
    let mut encoder = harness.encoder_with_video_cleared_to(wgpu::Color::BLACK);
    harness
        .compositor
        .render_retained_overlay(&mut encoder, &harness.view);

    let (paint_jobs, textures_delta) = tessellated_ui_frame();
    let upload = harness.compositor.upload_changed_textures(
        &harness.device,
        &harness.queue,
        &textures_delta,
    );
    assert_eq!(upload, super::UiTextureUpload::InSync);
    let _callbacks = harness.compositor.prepare_fresh_frame(
        &harness.device,
        &harness.queue,
        &mut encoder,
        paint_jobs,
        screen_descriptor(),
    );
    harness.compositor.free_retired_textures(textures_delta);
    drop(encoder);

    let resized = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [TARGET_WIDTH * 2, TARGET_HEIGHT],
        pixels_per_point: 1.0,
    };
    let rescaled = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [TARGET_WIDTH, TARGET_HEIGHT],
        pixels_per_point: 2.0,
    };
    assert_eq!(
        harness.compositor.retained_frame_availability(&screen),
        Ok(())
    );
    assert_eq!(
        harness.compositor.retained_frame_availability(&resized),
        Err(RetainedEguiFrameUnavailable::ScreenMismatch)
    );
    assert_eq!(
        harness.compositor.retained_frame_availability(&rescaled),
        Err(RetainedEguiFrameUnavailable::ScreenMismatch)
    );
}
