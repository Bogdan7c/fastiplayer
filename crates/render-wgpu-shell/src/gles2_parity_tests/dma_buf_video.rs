//! Пиксельный паритет аппаратного видео wgpu ↔ GLES2 (сессия 07 плана `user/gles2-renderer/`,
//! решения владельца 7 «строгий паритет» и 28 «тест живёт в шелле wgpu»).
//!
//! ОДИН И ТОТ ЖЕ кадр NV12 в буфере ядра (`memfd` → `/dev/udmabuf`, настоящий dma-buf fd)
//! рисуют:
//! - настоящий wgpu-путь: `DmaBufWgpuFrameMaterializer` (импорт Vulkan) → `WgpuVideoRenderer`
//!   (шейдер `nv12_to_rgba.wgsl`) с барьерами внешней очереди, как у шелла;
//! - GLES2: `VideoLayerRenderer::import_dma_buf_frame` (EGLImage) → `draw` по плану
//!   `VideoDrawPlan` (тот же `render-core`, что у wgpu).
//!
//! Сцены: composed и separate раскладки, letterbox, поворот под сайдбаром, сжатие, BT.601 full
//! с ручками цветокоррекции. Допуск ±2 на канал, как у паритета программного видео; расхождения
//! выше допуска — провал с координатой и обоими значениями.
//!
//! Пропуски (явная строка `skip …` в stderr): нет Vulkan-адаптера, нет EGL (но с
//! `GLES2_EGUI_REQUIRE_EGL` — падение), нет `/dev/udmabuf`, устройство wgpu не импортирует
//! DMA-BUF NV12.

use std::collections::BTreeMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use codec_core::{ColorRange, MatrixCoefficients, VideoColorMetadata, VideoDisplayOrientation};
use gles2_egui::{DrmFourcc, FrameStart};
use gles2_video::{
    DmaBufBufferIdentity, DmaBufFrameFormat, DmaBufImportOutcome, DmaBufNv12Frame,
    DmaBufObjectIdentity as Gles2ObjectIdentity, DmaBufPlane, DmaBufPlaneArrangement, PlaneExtent,
    VideoFrameKey, VideoLayerOptions, VideoLayerRenderer,
};
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrToSdrSettings, RenderLiveSettings, RenderViewport,
};
use render_gles2_shell::{VideoDrawPlan, VideoPlacement, renderable_frame};
use render_wgpu_video::{
    DmaBufImportCacheCapacity, DmaBufWgpuFrameMaterializer, WgpuFrameTextureViewLookup,
    WgpuFrameTextureViewMaterializer, WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
    create_video_device_with_external_sync, required_wgpu_video_texture_features,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, DmaBufFrameDescriptor, DmaBufFrameExportLayout, DmaBufLayerDescriptor,
    DmaBufObjectDescriptor, DmaBufObjectIdentity, FrameResourceDescriptor, FrameResourceHandle,
    VideoFrameDiagnostics,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::udmabuf::Udmabuf;
use super::wgpu_target::{TARGET_FORMAT, read_back_texture_around};
use super::{CHANNEL_TOLERANCE, compare, gles2_or_skip};

/// `DRM_FORMAT_NV12` / `R8` / `GR88` (`drm_fourcc.h`).
const DRM_FORMAT_NV12: u32 = 0x3231_564e;
const DRM_FORMAT_R8: u32 = 0x2020_3852;
const DRM_FORMAT_GR88: u32 = 0x3838_5247;
/// Шаг строк буфера кратен 256 (требование выравнивания AMD); хвост строки — заметный мусор,
/// который в картинку попасть не должен.
const PITCH_ALIGNMENT: u32 = 256;
const PADDING_BYTE: u8 = 0xEE;
/// Модификатор linear: udmabuf всегда linear.
const MODIFIER_LINEAR: u64 = 0;
const RESOURCE_ID: u64 = 1;

/// Раскладка плоскостей в буферах ядра.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferArrangement {
    /// Яркость и цветность в одном буфере (цветность — за яркостью).
    Composed,
    /// Два отдельных буфера.
    Separate,
}

/// Сцена паритета: кадр, раскладка, размещение и цвет.
struct DmaBufScene {
    name: &'static str,
    arrangement: BufferArrangement,
    frame_size: [u32; 2],
    orientation: VideoDisplayOrientation,
    color: VideoColorMetadata,
    adjustment: ColorAdjustment,
    exclusions: Vec<RenderViewport>,
}

impl DmaBufScene {
    fn pitch(&self) -> u32 {
        self.frame_size[0].div_ceil(PITCH_ALIGNMENT) * PITCH_ALIGNMENT
    }

    fn chroma_size(&self) -> [u32; 2] {
        [
            self.frame_size[0].div_ceil(2),
            self.frame_size[1].div_ceil(2),
        ]
    }

    /// Смещение цветности внутри буфера: за яркостью (composed) или с нуля (separate).
    fn chroma_offset(&self) -> u32 {
        match self.arrangement {
            BufferArrangement::Composed => self.pitch() * self.frame_size[1],
            BufferArrangement::Separate => 0,
        }
    }
}

/// Четыре блока известных цветов (Y, U, V) верхней половины кадра.
const BLOCK_COLORS: [[u32; 3]; 4] = [[81, 90, 240], [145, 54, 34], [41, 240, 110], [170, 166, 16]];

/// Яркость в точке: сверху — блоки, снизу — плавный градиент (проверяет билинейку).
fn luma_sample(scene: &DmaBufScene, x: u32, y: u32) -> u8 {
    let [width, height] = scene.frame_size;
    let value = if y < height / 2 {
        BLOCK_COLORS[(x * 4 / width) as usize][0]
    } else {
        16 + x * 190 / width + (y - height / 2) * 20 / (height / 2)
    };
    u8::try_from(value).unwrap_or(u8::MAX)
}

/// Пара цветности (U, V) в точке плоскости цветности.
fn chroma_sample(scene: &DmaBufScene, x: u32, y: u32) -> [u8; 2] {
    let [width, height] = scene.chroma_size();
    let (blue, red) = if y < height / 2 {
        let block = BLOCK_COLORS[(x * 4 / width) as usize];
        (block[1], block[2])
    } else {
        (
            64 + x * 128 / width,
            224 - x * 96 / width - (y - height / 2) * 40 / (height / 2),
        )
    };
    [
        u8::try_from(blue).unwrap_or(u8::MAX),
        u8::try_from(red).unwrap_or(u8::MAX),
    ]
}

fn luma_bytes(scene: &DmaBufScene) -> Vec<u8> {
    let [width, height] = scene.frame_size;
    let pitch = scene.pitch();
    let mut bytes = vec![PADDING_BYTE; (pitch * height) as usize];
    for y in 0..height {
        for x in 0..width {
            bytes[(y * pitch + x) as usize] = luma_sample(scene, x, y);
        }
    }
    bytes
}

fn chroma_bytes(scene: &DmaBufScene) -> Vec<u8> {
    let [width, height] = scene.chroma_size();
    let pitch = scene.pitch();
    let mut bytes = vec![PADDING_BYTE; (pitch * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let at = (y * pitch + x * 2) as usize;
            // GR88: первый байт — R (= U), второй — G (= V).
            bytes[at..at + 2].copy_from_slice(&chroma_sample(scene, x, y));
        }
    }
    bytes
}

/// Кадр сцены в буферах ядра: один для обоих рендеров.
struct SceneBuffers {
    buffers: Vec<Udmabuf>,
}

impl SceneBuffers {
    /// `Err` — причина, по которой нет `/dev/udmabuf`.
    fn create(scene: &DmaBufScene) -> Result<Self, String> {
        let buffers = match scene.arrangement {
            BufferArrangement::Composed => {
                let mut contents = luma_bytes(scene);
                contents.extend(chroma_bytes(scene));
                vec![Udmabuf::create(&contents)?]
            }
            BufferArrangement::Separate => vec![
                Udmabuf::create(&luma_bytes(scene))?,
                Udmabuf::create(&chroma_bytes(scene))?,
            ],
        };
        Ok(Self { buffers })
    }

    fn chroma_buffer(&self) -> &Udmabuf {
        self.buffers.last().expect("scene has at least one buffer")
    }
}

/// Кадр декодера сцены с контрактом DMA-BUF NV12.
fn decoded_scene_frame(scene: &DmaBufScene) -> DecodedFrame {
    let image_layout = match scene.arrangement {
        BufferArrangement::Composed => DmaBufImageLayout::ComposedLayers,
        BufferArrangement::Separate => DmaBufImageLayout::SeparateLayers,
    };
    let [width, height] = scene.frame_size;
    DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::dma_buf_nv12(image_layout),
        width,
        height,
        render_width: width,
        render_height: height,
        display_orientation: scene.orientation,
        color: scene.color.clone(),
        resource_handle: FrameResourceHandle(RESOURCE_ID),
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Provider кадров стенда: отдаёт дубликаты fd буферов сцены, как декодер.
struct SceneDmaBufProvider {
    descriptor_template: DescriptorTemplate,
}

/// Всё, что нужно собрать `DmaBufFrameDescriptor` по запросу (fd клонируются каждый раз).
struct DescriptorTemplate {
    arrangement: BufferArrangement,
    frame_size: [u32; 2],
    pitch: u32,
    chroma_offset: u32,
    object_fds: Vec<OwnedFd>,
    object_sizes: Vec<u32>,
    object_identities: Vec<DmaBufObjectIdentity>,
}

impl DescriptorTemplate {
    fn new(scene: &DmaBufScene, buffers: &SceneBuffers) -> Self {
        let passports: Vec<_> = buffers.buffers.iter().map(Udmabuf::passport).collect();
        Self {
            arrangement: scene.arrangement,
            frame_size: scene.frame_size,
            pitch: scene.pitch(),
            chroma_offset: scene.chroma_offset(),
            object_fds: buffers
                .buffers
                .iter()
                .map(|buffer| buffer.dma_buf.try_clone().expect("dup dma-buf fd"))
                .collect(),
            object_sizes: passports
                .iter()
                .map(|passport| u32::try_from(passport.size).expect("object size fits u32"))
                .collect(),
            object_identities: passports
                .iter()
                .map(|passport| DmaBufObjectIdentity {
                    device: passport.device,
                    inode: passport.inode,
                    special_device: 0,
                })
                .collect(),
        }
    }

    fn descriptor(&self) -> DmaBufFrameDescriptor {
        let objects = self
            .object_fds
            .iter()
            .zip(&self.object_sizes)
            .zip(&self.object_identities)
            .map(|((fd, size), identity)| DmaBufObjectDescriptor {
                fd: fd.try_clone().expect("dup dma-buf fd"),
                size: *size,
                drm_format_modifier: MODIFIER_LINEAR,
                identity: *identity,
            })
            .collect();
        let luma_size = self.pitch * self.frame_size[1];
        let (export_layout, layers) = match self.arrangement {
            BufferArrangement::Composed => (
                DmaBufFrameExportLayout::ComposedLayers,
                vec![DmaBufLayerDescriptor {
                    drm_format: DRM_FORMAT_NV12,
                    num_planes: 2,
                    object_index: [0, 0, 0, 0],
                    offset: [0, luma_size, 0, 0],
                    pitch: [self.pitch, self.pitch, 0, 0],
                }],
            ),
            BufferArrangement::Separate => (
                DmaBufFrameExportLayout::SeparateLayers,
                vec![
                    DmaBufLayerDescriptor {
                        drm_format: DRM_FORMAT_R8,
                        num_planes: 1,
                        object_index: [0, 0, 0, 0],
                        offset: [0, 0, 0, 0],
                        pitch: [self.pitch, 0, 0, 0],
                    },
                    DmaBufLayerDescriptor {
                        drm_format: DRM_FORMAT_GR88,
                        num_planes: 1,
                        object_index: [1, 0, 0, 0],
                        offset: [self.chroma_offset, 0, 0, 0],
                        pitch: [self.pitch, 0, 0, 0],
                    },
                ],
            ),
        };
        DmaBufFrameDescriptor {
            resource_id: RESOURCE_ID,
            fourcc: DRM_FORMAT_NV12,
            export_layout,
            width: self.frame_size[0],
            height: self.frame_size[1],
            objects,
            layers,
        }
    }
}

impl PresentFrameResourceProvider for SceneDmaBufProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        let resource_pool_lock_wait = Duration::ZERO;
        if handle.0 == RESOURCE_ID {
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
        if handle.0 == RESOURCE_ID {
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::DmaBuf(self.descriptor_template.descriptor()),
                resource_pool_lock_wait,
            }
        } else {
            PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            }
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}

fn color_settings(adjustment: ColorAdjustment) -> ColorPipelineSettings {
    let mut settings = ColorPipelineSettings::identity();
    settings.adjustment = adjustment;
    settings
}

/// Устройство wgpu как у production-шелла: только Vulkan (GL-бэкенд wgpu делил бы `EGLDisplay`
/// с GLES2 и гасил его), video-features и внешняя синхронизация очередей. `None` — нет адаптера.
fn dma_buf_device() -> Option<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let instance = crate::shell::gpu_context::vulkan_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("gles2 dma-buf parity wgpu device"),
        required_features: required_wgpu_video_texture_features(&adapter),
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    };
    // Как `vulkan_device::create_video_device`: сначала с foreign queue family, иначе обычное.
    let (device, queue) = match create_video_device_with_external_sync(&adapter, &descriptor) {
        Ok(Some(device_and_queue)) => device_and_queue,
        Ok(None) => pollster::block_on(adapter.request_device(&descriptor))
            .expect("create wgpu device with video features"),
        Err(error) => panic!("wgpu device with external sync failed: {error}"),
    };
    Some((instance, adapter, device, queue))
}

/// Итог wgpu-стороны.
enum WgpuOutcome {
    Pixels(Vec<u8>),
    /// Нет адаптера или устройство не импортирует DMA-BUF NV12: причина для `skip`.
    Skipped(String),
}

/// wgpu-сторона: настоящий материализатор DMA-BUF и видеорендер, цель `Rgba8Unorm`.
fn render_wgpu(scene: &DmaBufScene, buffers: &SceneBuffers, size: [u32; 2]) -> WgpuOutcome {
    let Some((instance, adapter, device, queue)) = dma_buf_device() else {
        return WgpuOutcome::Skipped("no Vulkan adapter for wgpu".to_owned());
    };
    let provider = SceneDmaBufProvider {
        descriptor_template: DescriptorTemplate::new(scene, buffers),
    };
    let materializer = DmaBufWgpuFrameMaterializer::new(
        &instance,
        &adapter,
        &device,
        &queue,
        PresentFrameResourceProviderHandle::new(provider),
        DmaBufImportCacheCapacity::matching_surface_pool(1),
    );
    let frame = decoded_scene_frame(scene);
    let views = match materializer.try_texture_view_lookup(&frame) {
        WgpuFrameTextureViewLookup::Ready { views, .. } => views,
        WgpuFrameTextureViewLookup::Error { .. } => {
            return WgpuOutcome::Skipped(format!(
                "{}: wgpu device cannot import this DMA-BUF NV12 ({:?})",
                scene.name,
                adapter.get_info().name
            ));
        }
        WgpuFrameTextureViewLookup::Unsupported { reason, .. } => {
            panic!(
                "{}: wgpu rejects the frame contract: {reason:?}",
                scene.name
            )
        }
        WgpuFrameTextureViewLookup::Busy { .. } | WgpuFrameTextureViewLookup::Missing { .. } => {
            panic!("{}: wgpu lookup is busy or missing the frame", scene.name)
        }
    };
    let plane_views = views.dma_buf_views().expect("DMA-BUF plane views");
    let renderable =
        WgpuRenderableFrame::from_decoded_nv12(&frame, plane_views).expect("wgpu frame");
    let mut renderer = WgpuVideoRenderer::new(&device, TARGET_FORMAT);
    renderer.resize(size[0], size[1]);
    renderer.set_color_pipeline_settings(color_settings(scene.adjustment));
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gles2 dma-buf parity wgpu target"),
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
    // Барьеры внешней очереди: acquire — до кадра, release — после (как submit шелла).
    let (acquire, release) = match outcome.external_sync {
        Some(sync) => (vec![sync.acquire], vec![sync.release]),
        None => (Vec::new(), Vec::new()),
    };
    WgpuOutcome::Pixels(read_back_texture_around(
        &device, &queue, &texture, size, acquire, release, encoder,
    ))
}

/// GLES2-сторона: импорт DMA-BUF в EGLImage и рисование видеослоем. `None` — нет EGL.
fn render_gles2(scene: &DmaBufScene, buffers: &SceneBuffers, size: [u32; 2]) -> Option<Vec<u8>> {
    let mut context = gles2_or_skip(scene.name, size)?;
    let [width, height] = scene.frame_size;
    let luma_extent = PlaneExtent { width, height };
    let identity = DmaBufBufferIdentity {
        resource_id: RESOURCE_ID,
        objects: buffers
            .buffers
            .iter()
            .map(|buffer| {
                let passport = buffer.passport();
                Gles2ObjectIdentity {
                    device: passport.device,
                    inode: passport.inode,
                    size: passport.size,
                    modifier: MODIFIER_LINEAR,
                }
            })
            .collect(),
        format: DmaBufFrameFormat {
            luma_extent,
            arrangement: match scene.arrangement {
                BufferArrangement::Composed => DmaBufPlaneArrangement::ComposedSingleObject,
                BufferArrangement::Separate => DmaBufPlaneArrangement::SeparatePlanes,
            },
            modifier: MODIFIER_LINEAR,
        },
    };
    let plane = |fd, offset: u32, drm_format: DrmFourcc| DmaBufPlane {
        fd,
        offset: u64::from(offset),
        pitch: scene.pitch(),
        modifier: MODIFIER_LINEAR,
        drm_format: drm_format.0,
    };
    let frame = DmaBufNv12Frame::new(
        VideoFrameKey(1),
        &identity,
        luma_extent,
        plane(buffers.buffers[0].dma_buf.as_fd(), 0, DrmFourcc::R8),
        plane(
            buffers.chroma_buffer().dma_buf.as_fd(),
            scene.chroma_offset(),
            DrmFourcc::GR88,
        ),
    )
    .expect("valid DMA-BUF frame");
    let mut video =
        VideoLayerRenderer::new(&context, VideoLayerOptions::default()).expect("GLES2 video layer");
    let imported = video
        .import_dma_buf_frame(&context, &frame)
        .expect("GLES2 imports the DMA-BUF");
    assert!(
        matches!(imported, DmaBufImportOutcome::Imported(_)),
        "{}: {imported:?}",
        scene.name
    );
    let decoded = decoded_scene_frame(scene);
    let plan = VideoDrawPlan::for_frame(
        &renderable_frame(&decoded).expect("YUV frame"),
        VideoPlacement {
            surface_size: (size[0], size[1]),
            video_viewport: RenderViewport::full_surface(size[0], size[1]),
            exclusions: &scene.exclusions,
        },
        &RenderLiveSettings {
            color_pipeline: color_settings(scene.adjustment),
            hdr_to_sdr: HdrToSdrSettings::default(),
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

fn scenes() -> Vec<DmaBufScene> {
    let sdr_709_limited = VideoColorMetadata::sdr_bt709_limited();
    let sdr_601_full = VideoColorMetadata {
        range: ColorRange::Full,
        matrix: MatrixCoefficients::Bt601,
        ..VideoColorMetadata::sdr_bt709_limited()
    };
    let base = |name, arrangement, frame_size| DmaBufScene {
        name,
        arrangement,
        frame_size,
        orientation: VideoDisplayOrientation::Identity,
        color: sdr_709_limited.clone(),
        adjustment: ColorAdjustment::identity(),
        exclusions: Vec::new(),
    };
    vec![
        base(
            "composed 64x36 stretched",
            BufferArrangement::Composed,
            [64, 36],
        ),
        base(
            "separate 64x36 stretched",
            BufferArrangement::Separate,
            [64, 36],
        ),
        base(
            "composed 64x48 letterboxed",
            BufferArrangement::Composed,
            [64, 48],
        ),
        DmaBufScene {
            orientation: VideoDisplayOrientation::Rotate90Clockwise,
            exclusions: vec![RenderViewport::new(0, 0, 40, 90)],
            ..base(
                "composed 64x48 rotated 90 under a sidebar",
                BufferArrangement::Composed,
                [64, 48],
            )
        },
        base(
            "separate 320x180 downscaled",
            BufferArrangement::Separate,
            [320, 180],
        ),
        DmaBufScene {
            color: sdr_601_full,
            adjustment: ColorAdjustment {
                brightness: 0.04,
                contrast: 1.2,
                saturation: 1.35,
                exposure: -0.25,
                rgb_gain: [1.05, 0.95, 1.1],
                rgb_offset: [0.01, -0.02, 0.0],
            },
            ..base(
                "composed 64x36 bt601 full with every adjustment",
                BufferArrangement::Composed,
                [64, 36],
            )
        },
    ]
}

/// Один и тот же DMA-BUF кадр NV12 выглядит одинаково через wgpu и через GLES2 во всех сценах.
#[test]
fn dma_buf_nv12_frames_match_wgpu_pixel_for_pixel() {
    let test_name = "dma_buf_nv12_frames_match_wgpu_pixel_for_pixel";
    let size = [160, 90];
    let mut reports = BTreeMap::new();
    for scene in scenes() {
        let buffers = match SceneBuffers::create(&scene) {
            Ok(buffers) => buffers,
            Err(reason) => {
                eprintln!("skip {test_name}: {reason}");
                return;
            }
        };
        let wgpu_pixels = match render_wgpu(&scene, &buffers, size) {
            WgpuOutcome::Pixels(pixels) => pixels,
            WgpuOutcome::Skipped(reason) => {
                eprintln!("skip {test_name}: {reason}");
                return;
            }
        };
        let Some(gles2_pixels) = render_gles2(&scene, &buffers, size) else {
            return;
        };
        let report = compare(&wgpu_pixels, &gles2_pixels, size[0]);
        let total_pixels = size[0] * size[1];
        eprintln!(
            "dma-buf video parity {}: max Δ {}, differing {} / {total_pixels} ({:.2} %), colors {}",
            scene.name,
            report.max_channel_difference,
            report.pixels_differing,
            100.0 * report.pixels_differing as f64 / f64::from(total_pixels),
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
        reports.insert(scene.name, report.max_channel_difference);
    }
    assert_eq!(reports.len(), scenes().len(), "every scene was compared");
}
