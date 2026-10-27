//! Пиксельный паритет видео wgpu ↔ GLES2 (сессия 05 плана `user/gles2-renderer/`).
//!
//! Один и тот же кадр программного декодера (`DecodedFrame` + `HostPlanarFrameDescriptor`)
//! рисуют:
//! - настоящий wgpu-путь: `HostPlanarWgpuFrameMaterializer` → `WgpuVideoRenderer::render_or_clear`
//!   (копия загрузки — первой в submit кадра, как в шелле);
//! - GLES2: адаптер `render-gles2-shell` → видеослой `gles2-video` на EGL без окна.
//!
//! Сцены покрывают билинейную яркость при растяжении, выбор цветности 4:2:0/4:2:2/4:4:4,
//! letterbox, поворот, исключение под сайдбаром, BT.601/709, limited/full, все ручки
//! цветокоррекции, stride с хвостом и текстуру шириной в stride (путь без `unpack_subimage`).
//! Допуск ±2 на канал; пиксели за допуском — провал с координатой и обоими значениями.
//!
//! Стенд общий с паритетом 10/12 бит и HDR (`video_high_bit.rs`, сессия 06): кадры любой
//! разрядности строятся одним узором, у сцены — свои настройки HDR → SDR.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use codec_core::{ColorRange, MatrixCoefficients, VideoColorMetadata, VideoDisplayOrientation};
use gles2_egui::FrameStart;
use gles2_video::{RowLengthPreference, UploadOutcome, VideoLayerOptions, VideoLayerRenderer};
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrToSdrSettings, RenderLiveSettings, RenderViewport,
};
use render_gles2_shell::{
    VideoDrawPlan, VideoPlacement, host_planar_frame_upload, renderable_frame,
};
use render_wgpu_video::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, WgpuRenderableFrame,
    WgpuVideoRenderInput, WgpuVideoRenderer,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneDescriptor, HostPlaneRole, VideoFrameDiagnostics,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

use super::wgpu_target::{TARGET_FORMAT, parity_device, read_back_texture_after};
use super::{CHANNEL_TOLERANCE, compare, gles2_or_skip};

/// Сцена паритета: кадр, его размещение и настройки цвета.
pub(super) struct VideoScene {
    pub(super) name: &'static str,
    pub(super) layout: VideoFramePixelLayout,
    pub(super) frame_size: [u32; 2],
    /// Отсчётов мусора в конце каждой строки (stride больше видимой части).
    pub(super) row_tail: usize,
    pub(super) orientation: VideoDisplayOrientation,
    pub(super) color: VideoColorMetadata,
    pub(super) adjustment: ColorAdjustment,
    pub(super) hdr_to_sdr: HdrToSdrSettings,
    pub(super) exclusions: Vec<RenderViewport>,
    pub(super) gles2_row_length: RowLengthPreference,
}

/// Отсчёт плоскости: гладкие градиенты (проверяют билинейную яркость) и резкая клетка
/// цветности (проверяет выбор отсчёта цветности).
fn sample(role: HostPlaneRole, x: u32, y: u32) -> u8 {
    let value = match role {
        HostPlaneRole::Luma => 16 + (x * 7 + y * 3) % 220,
        HostPlaneRole::ChromaU => {
            if (x / 2 + y).is_multiple_of(2) {
                60 + (x * 5) % 120
            } else {
                200
            }
        }
        HostPlaneRole::ChromaV => 40 + (x * 11 + y * 13) % 180,
    };
    u8::try_from(value).unwrap_or(u8::MAX)
}

fn chroma_size(layout: VideoFramePixelLayout, [width, height]: [u32; 2]) -> [u32; 2] {
    match layout {
        VideoFramePixelLayout::Yuv420Planar8
        | VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv420Planar12Le => [width.div_ceil(2), height.div_ceil(2)],
        VideoFramePixelLayout::Yuv422Planar8
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv422Planar12Le => [width.div_ceil(2), height],
        _ => [width, height],
    }
}

/// Значащих бит отсчёта раскладки сцены.
fn sample_bits(layout: VideoFramePixelLayout) -> u32 {
    match layout {
        VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv444Planar10Le => 10,
        VideoFramePixelLayout::Yuv420Planar12Le | VideoFramePixelLayout::Yuv422Planar12Le => 12,
        _ => 8,
    }
}

/// Байты отсчёта: 8 бит — сам узор; 10/12 — узор в старших битах и меняющиеся младшие биты
/// (проверяют, что младший байт пары доходит до цвета), 16-битное слово LE.
fn sample_bytes(layout: VideoFramePixelLayout, role: HostPlaneRole, x: u32, y: u32) -> Vec<u8> {
    let bits = sample_bits(layout);
    let base = u32::from(sample(role, x, y));
    if bits == 8 {
        return vec![sample(role, x, y)];
    }
    let shift = bits - 8;
    let low_bits = (x * 3 + y * 5) % (1 << shift);
    let code = u16::try_from((base << shift) | low_bits).unwrap_or(u16::MAX);
    code.to_le_bytes().to_vec()
}

/// Кадр декодера сцены: плоскости как у FFmpeg (блок кончается на видимой части последней
/// строки, хвосты строк — мусор `0xFF`).
fn decoded_scene_frame(
    scene: &VideoScene,
    handle: FrameResourceHandle,
) -> (DecodedFrame, HostPlanarFrameDescriptor) {
    let [width, height] = scene.frame_size;
    let [chroma_width, chroma_height] = chroma_size(scene.layout, scene.frame_size);
    let bytes_per_sample = if sample_bits(scene.layout) == 8 { 1 } else { 2 };
    let mut bytes = Vec::new();
    let mut planes = Vec::new();
    for (role, plane_width, plane_height) in [
        (HostPlaneRole::Luma, width, height),
        (HostPlaneRole::ChromaU, chroma_width, chroma_height),
        (HostPlaneRole::ChromaV, chroma_width, chroma_height),
    ] {
        let offset = bytes.len();
        for row in 0..plane_height {
            for x in 0..plane_width {
                bytes.extend(sample_bytes(scene.layout, role, x, row));
            }
            if row + 1 < plane_height {
                bytes.extend(std::iter::repeat_n(0xFF, scene.row_tail * bytes_per_sample));
            }
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride: (plane_width as usize + scene.row_tail) * bytes_per_sample,
            visible_width: plane_width,
            visible_height: plane_height,
            bytes_per_sample,
        });
    }
    let frame = DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract {
            pixel_layout: scene.layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        width,
        height,
        render_width: width,
        render_height: height,
        display_orientation: scene.orientation,
        color: scene.color.clone(),
        resource_handle: handle,
        diagnostics: VideoFrameDiagnostics::default(),
    };
    (
        frame,
        HostPlanarFrameDescriptor::from_owned_buffer(bytes, planes),
    )
}

/// Provider кадров стенда: отдаёт дескриптор по handle, как декодер.
#[derive(Debug, Default)]
struct SceneFrameProvider {
    descriptors: BTreeMap<u64, HostPlanarFrameDescriptor>,
}

impl PresentFrameResourceProvider for SceneFrameProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        let resource_pool_lock_wait = Duration::ZERO;
        if self.descriptors.contains_key(&handle.0) {
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait,
            }
        } else {
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait,
            }
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let resource_pool_lock_wait = Duration::ZERO;
        match self.descriptors.get(&handle.0) {
            Some(descriptor) => PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::HostPlanar(descriptor.clone()),
                resource_pool_lock_wait,
            },
            None => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}

fn color_settings(adjustment: ColorAdjustment) -> ColorPipelineSettings {
    let mut settings = ColorPipelineSettings::identity();
    settings.adjustment = adjustment;
    settings
}

/// wgpu-сторона: настоящий материализатор и видеорендер, цель `Rgba8Unorm`.
pub(super) fn render_wgpu(scene: &VideoScene, size: [u32; 2]) -> Option<Vec<u8>> {
    let (device, queue) = parity_device()?;
    let handle = FrameResourceHandle(1);
    let (frame, descriptor) = decoded_scene_frame(scene, handle);
    let mut provider = SceneFrameProvider::default();
    provider.descriptors.insert(handle.0, descriptor);
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        &device,
        &queue,
        PresentFrameResourceProviderHandle::new(provider),
    );
    let HostPlanarWgpuTextureViewLookup::Ready { views, .. } =
        materializer.try_host_planar_texture_view_lookup(&frame)
    else {
        panic!("{}: wgpu materializer must upload the frame", scene.name);
    };
    let renderable = WgpuRenderableFrame::from_decoded_host_planar(&frame, views.plane_views())
        .expect("wgpu frame");
    let mut renderer = WgpuVideoRenderer::new(&device, TARGET_FORMAT);
    renderer.resize(size[0], size[1]);
    renderer.set_color_pipeline_settings(color_settings(scene.adjustment));
    renderer.set_hdr_to_sdr_settings(scene.hdr_to_sdr);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gles2 video parity wgpu target"),
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
    let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let outcome = renderer
        .render_or_clear(WgpuVideoRenderInput {
            frame: Some(&renderable),
            video_viewport: RenderViewport::full_surface(size[0], size[1]),
            video_exclusion_rects: &scene.exclusions,
            target: &target,
            encoder: &mut encoder,
            device: &device,
            queue: &queue,
        })
        .expect("wgpu draws the frame");
    assert!(outcome.frame_drawn, "{}: wgpu frame drawn", scene.name);
    let upload = outcome
        .host_upload
        .map(|commands| commands.into_command_buffer())
        .into_iter()
        .collect();
    Some(read_back_texture_after(
        &device, &queue, &texture, size, upload, encoder,
    ))
}

/// GLES2-сторона: адаптер плеера и видеослой.
pub(super) fn render_gles2(scene: &VideoScene, size: [u32; 2]) -> Option<Vec<u8>> {
    let mut context = gles2_or_skip(scene.name, size)?;
    let (frame, descriptor) = decoded_scene_frame(scene, FrameResourceHandle(1));
    let descriptor = FrameResourceDescriptor::HostPlanar(descriptor);
    let mut video = VideoLayerRenderer::new(
        &context,
        VideoLayerOptions {
            row_length: scene.gles2_row_length,
            ..VideoLayerOptions::default()
        },
    )
    .expect("GLES2 video layer");
    let upload = host_planar_frame_upload(&frame, &descriptor).expect("adapter maps the frame");
    let outcome = video.upload(&context, &upload).expect("GLES2 upload");
    assert!(
        matches!(outcome, UploadOutcome::Uploaded(_)),
        "{}: {outcome:?}",
        scene.name
    );
    let plan = VideoDrawPlan::for_frame(
        &renderable_frame(&frame).expect("YUV frame"),
        VideoPlacement {
            surface_size: (size[0], size[1]),
            video_viewport: RenderViewport::full_surface(size[0], size[1]),
            exclusions: &scene.exclusions,
        },
        &RenderLiveSettings {
            color_pipeline: color_settings(scene.adjustment),
            hdr_to_sdr: scene.hdr_to_sdr,
            ..RenderLiveSettings::default()
        },
    )
    .expect("draw plan");
    match context.begin_frame().expect("GLES2 frame") {
        FrameStart::Ready(active) => {
            video
                .draw(&active, &plan.request())
                .expect("GLES2 draws the frame");
            active.present().expect("GLES2 present");
        }
        FrameStart::Skipped(reason) => panic!("GLES2 frame skipped: {reason:?}"),
    }
    video.retire_submitted_work(Arc::new(context.insert_fence().expect("fence")));
    let pixels = context
        .read_offscreen_pixels()
        .expect("GLES2 readback")
        .as_rgba()
        .to_vec();
    video.destroy(&context).expect("destroy video layer");
    Some(pixels)
}

fn scenes() -> Vec<VideoScene> {
    let sdr_709_limited = VideoColorMetadata::sdr_bt709_limited();
    let sdr_601_full = VideoColorMetadata {
        range: ColorRange::Full,
        matrix: MatrixCoefficients::Bt601,
        ..VideoColorMetadata::sdr_bt709_limited()
    };
    let base = |name, layout| VideoScene {
        name,
        layout,
        frame_size: [61, 35],
        row_tail: 0,
        orientation: VideoDisplayOrientation::Identity,
        color: sdr_709_limited.clone(),
        adjustment: ColorAdjustment::identity(),
        hdr_to_sdr: HdrToSdrSettings::default(),
        exclusions: Vec::new(),
        gles2_row_length: RowLengthPreference::BestAvailable,
    };
    vec![
        base(
            "420 bt709 limited upscaled",
            VideoFramePixelLayout::Yuv420Planar8,
        ),
        base(
            "422 bt709 limited upscaled",
            VideoFramePixelLayout::Yuv422Planar8,
        ),
        base(
            "444 bt709 limited upscaled",
            VideoFramePixelLayout::Yuv444Planar8,
        ),
        VideoScene {
            color: sdr_601_full.clone(),
            adjustment: ColorAdjustment {
                brightness: 0.04,
                contrast: 1.2,
                saturation: 1.35,
                exposure: -0.25,
                rgb_gain: [1.05, 0.95, 1.1],
                rgb_offset: [0.01, -0.02, 0.0],
            },
            ..base(
                "420 bt601 full with every adjustment",
                VideoFramePixelLayout::Yuv420Planar8,
            )
        },
        VideoScene {
            orientation: VideoDisplayOrientation::Rotate90Clockwise,
            exclusions: vec![RenderViewport::new(0, 0, 40, 90)],
            ..base(
                "420 rotated 90 under a sidebar",
                VideoFramePixelLayout::Yuv420Planar8,
            )
        },
        VideoScene {
            orientation: VideoDisplayOrientation::Rotate270Clockwise,
            frame_size: [300, 120],
            ..base(
                "444 rotated 270 downscaled",
                VideoFramePixelLayout::Yuv444Planar8,
            )
        },
        VideoScene {
            row_tail: 35,
            ..base("420 padded stride", VideoFramePixelLayout::Yuv420Planar8)
        },
        VideoScene {
            row_tail: 35,
            gles2_row_length: RowLengthPreference::StrideWideTextures,
            ..base(
                "420 padded stride, stride-wide GLES2 textures",
                VideoFramePixelLayout::Yuv420Planar8,
            )
        },
        VideoScene {
            row_tail: 3,
            gles2_row_length: RowLengthPreference::StrideWideTextures,
            ..base(
                "422 padded stride, stride-wide GLES2 textures",
                VideoFramePixelLayout::Yuv422Planar8,
            )
        },
    ]
}

/// Кадр декодера одинаково выглядит через wgpu и через GLES2 во всех сценах.
#[test]
fn software_video_frames_match_wgpu_pixel_for_pixel() {
    let size = [160, 90];
    for scene in scenes() {
        let Some(wgpu_pixels) = render_wgpu(&scene, size) else {
            eprintln!("skip video parity: no Vulkan adapter for wgpu");
            return;
        };
        let Some(gles2_pixels) = render_gles2(&scene, size) else {
            return;
        };
        let report = compare(&wgpu_pixels, &gles2_pixels, size[0]);
        eprintln!(
            "video parity {}: max Δ {}, differing {} / {}, colors {}",
            scene.name,
            report.max_channel_difference,
            report.pixels_differing,
            size[0] * size[1],
            report.distinct_colors
        );
        assert!(
            report.distinct_colors > 200,
            "{}: scene must exercise many colours, got {}",
            scene.name,
            report.distinct_colors
        );
        assert_eq!(
            report.pixels_over_tolerance, 0,
            "{}: {} pixels over ±{CHANNEL_TOLERANCE}, first (x, y, wgpu, gles2) = {:?}",
            scene.name, report.pixels_over_tolerance, report.first_over_tolerance
        );
    }
}
