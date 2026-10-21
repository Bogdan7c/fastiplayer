//! GPU-тесты: настоящий `WgpuVideoRenderer::render_or_clear` на host-planar пути.
//!
//! Нужен любой Vulkan/GL адаптер (специальных фич не требуется). Если адаптера
//! нет, тест сообщает об этом и пропускается — по образцу остальных GPU-тестов crate-а.

use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use render_core::RenderViewport;
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::VideoFrameContract;

use super::*;
use crate::video::{
    WgpuHostPlanarPlaneViews, WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
};

const FRAME_WIDTH: u32 = 64;
const FRAME_HEIGHT: u32 = 32;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Device/queue для теста либо `None`, если адаптера в окружении нет.
fn try_create_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Слот host-planar загрузки: три R8 плоскости и его кэш bind group-ов.
struct FakeUploadSlot {
    y_view: wgpu::TextureView,
    u_view: wgpu::TextureView,
    v_view: wgpu::TextureView,
    bind_groups: FrameBindGroupCache,
}

impl FakeUploadSlot {
    fn new(device: &wgpu::Device) -> Self {
        let plane_view = |width: u32, height: u32| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("bind group cache test plane"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        Self {
            y_view: plane_view(FRAME_WIDTH, FRAME_HEIGHT),
            u_view: plane_view(FRAME_WIDTH / 2, FRAME_HEIGHT / 2),
            v_view: plane_view(FRAME_WIDTH / 2, FRAME_HEIGHT / 2),
            bind_groups: FrameBindGroupCache::default(),
        }
    }

    fn plane_views(&self) -> WgpuHostPlanarPlaneViews<'_> {
        WgpuHostPlanarPlaneViews {
            y_view: &self.y_view,
            u_view: &self.u_view,
            v_view: &self.v_view,
            bind_group_cache: Some(&self.bind_groups),
        }
    }
}

fn decoded_host_frame() -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        render_width: FRAME_WIDTH,
        render_height: FRAME_HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(1),
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Рисует один кадр через публичный `render_or_clear` и отправляет его в очередь.
fn render_and_submit(
    gpu: &(wgpu::Device, wgpu::Queue),
    renderer: &mut WgpuVideoRenderer,
    target: &wgpu::TextureView,
    frame: &WgpuRenderableFrame<'_>,
    viewport_width: u32,
) {
    let (device, queue) = gpu;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let outcome = renderer
        .render_or_clear(WgpuVideoRenderInput {
            frame: Some(frame),
            video_viewport: RenderViewport {
                x: 0,
                y: 0,
                width: viewport_width,
                height: FRAME_HEIGHT,
            },
            video_exclusion_rects: &[],
            target,
            encoder: &mut encoder,
            device,
            queue,
        })
        .expect("renderer accepts host-planar frame");
    assert!(outcome.frame_drawn, "кадр должен дойти до render pass-а");
    queue.submit([encoder.finish()]);
}

#[test]
fn steady_stream_over_two_upload_slots_creates_no_bind_groups_and_writes_no_uniforms() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let target = gpu
        .0
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("bind group cache test target"),
            size: wgpu::Extent3d {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = WgpuVideoRenderer::new(&gpu.0, TARGET_FORMAT);
    renderer.resize(FRAME_WIDTH, FRAME_HEIGHT);
    let slots = [FakeUploadSlot::new(&gpu.0), FakeUploadSlot::new(&gpu.0)];
    let decoded = decoded_host_frame();

    // Кадры идут по кругу через два слота, viewport не меняется.
    for frame_index in 0..6_usize {
        let slot = &slots[frame_index % slots.len()];
        let frame = WgpuRenderableFrame::from_decoded_host_planar(&decoded, slot.plane_views())
            .expect("host-planar frame");
        render_and_submit(&gpu, &mut renderer, &target, &frame, FRAME_WIDTH);
    }

    assert_eq!(
        renderer.hot_path_stats(),
        VideoHotPathStats {
            // По одной bind group на слот загрузки, дальше только повторы.
            bind_groups_created: 2,
            bind_groups_reused: 4,
            // Uniform записан один раз, остальные 5 кадров байты те же.
            uniform_writes: 1,
            uniform_writes_skipped: 5,
        }
    );

    // Смена viewport меняет letterbox-uniform: запись обязана случиться,
    // а bind group остаётся прежней.
    let slot = &slots[0];
    let frame = WgpuRenderableFrame::from_decoded_host_planar(&decoded, slot.plane_views())
        .expect("host-planar frame");
    render_and_submit(&gpu, &mut renderer, &target, &frame, FRAME_WIDTH / 2);
    let after_resize = renderer.hot_path_stats();
    assert_eq!(after_resize.uniform_writes, 2);
    assert_eq!(after_resize.bind_groups_created, 2);
    assert_eq!(after_resize.bind_groups_reused, 5);
}

#[test]
fn frame_without_cache_owner_keeps_per_frame_bind_group_creation() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let target = gpu
        .0
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("bind group cache test target"),
            size: wgpu::Extent3d {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = WgpuVideoRenderer::new(&gpu.0, TARGET_FORMAT);
    renderer.resize(FRAME_WIDTH, FRAME_HEIGHT);
    let slot = FakeUploadSlot::new(&gpu.0);
    let decoded = decoded_host_frame();

    for _ in 0..3 {
        // Старый конструктор: views по отдельности, кэша нет.
        let frame = WgpuRenderableFrame::from_decoded_host_yuv(
            &decoded,
            &slot.y_view,
            &slot.u_view,
            &slot.v_view,
        )
        .expect("host-planar frame");
        render_and_submit(&gpu, &mut renderer, &target, &frame, FRAME_WIDTH);
    }

    let stats = renderer.hot_path_stats();
    assert_eq!(stats.bind_groups_created, 3);
    assert_eq!(stats.bind_groups_reused, 0);
    assert_eq!(slot.bind_groups.len(), 0, "кэш слота не должен наполняться");
    // Гейт uniform работает независимо от наличия кэша.
    assert_eq!(stats.uniform_writes, 1);
    assert_eq!(stats.uniform_writes_skipped, 2);
}
