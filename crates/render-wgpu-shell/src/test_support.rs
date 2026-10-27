//! Общий стенд тестов настоящего wgpu-рендера без окна (сессия 02 `user/gles2-renderer/`).
//!
//! - [`read_texture_pixel`] — readback одного пикселя offscreen-цели, [`read_texture_frame`] —
//!   всего кадра (пиксельные тесты интерфейса, сессия 02a);
//! - [`SolidFrameDecoder`] — «декодер» без декодирования: provider отдаёт HostPlanar-кадр
//!   4×2 (белый для [`WHITE_FRAME`], чёрный для остальных) и записывает каждое освобождение;
//! - [`lease_for`] — аренда кадра, как её строит плеер (освобождение уходит в provider).

use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::Renderer;
use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use render_core::RenderViewport;
use render_shell_api::conformance::ConformanceReleaseLedger;
use render_shell_api::{
    DecoderSurfacePoolSlots, EguiFrameContent, FrameMaterializationOutcome, RenderFrameInput,
    RenderFrameOutcome, RenderScreenDescriptor, RenderShell, RenderableFrameKey,
    SurfaceFrameAcquire, VideoImportPath, VideoImportPlan,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup, StartedVideoBackend,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor, HostPlaneDescriptor,
    HostPlaneRole, VideoDecoderDiagnosticEvent, VideoDecoderThreadHandle, VideoFrameDiagnostics,
};
use video_frame_contract::VideoFrameContract;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

/// План программного декода стенда (пул декодера на путь загрузки не влияет).
pub(crate) const SOFTWARE_PLAN: VideoImportPlan = VideoImportPlan {
    path: VideoImportPath::SoftwareHostUpload,
    decoder_surface_pool: DecoderSurfacePoolSlots::new(8),
};

/// Рендер без окна либо `None` с напечатанной причиной пропуска.
pub(crate) fn headless_renderer_or_skip(test_name: &str) -> Option<Renderer> {
    match Renderer::new_headless(HEADLESS_SIZE) {
        Ok(Some(renderer)) => Some(renderer),
        Ok(None) => {
            eprintln!("skip {test_name}: no Vulkan adapter for the headless wgpu renderer");
            None
        }
        Err(error) => panic!("{test_name}: headless wgpu renderer creation failed: {error:#}"),
    }
}

/// Экран стенда: весь offscreen-target, масштаб 1.
pub(crate) const fn headless_screen() -> RenderScreenDescriptor {
    RenderScreenDescriptor {
        size_in_pixels: HEADLESS_SIZE,
        pixels_per_point: 1.0,
    }
}

/// Получает буфер и показывает кадр с видео (или без) поверх пустого интерфейса.
pub(crate) fn present(
    renderer: &mut dyn RenderShell,
    video_frame: Option<&RenderableFrameKey>,
) -> RenderFrameOutcome {
    let screen = headless_screen();
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        panic!("offscreen target must always give a buffer");
    };
    renderer.render_frame(RenderFrameInput {
        surface,
        video_frame,
        egui: EguiFrameContent::empty_fresh(),
        screen,
        video_viewport: RenderViewport::full_surface(HEADLESS_SIZE[0], HEADLESS_SIZE[1]),
        video_exclusion_rects: Vec::new(),
        subtitle_overlay: None,
    })
}

/// Ключ из исхода `Ready`, иначе — падение теста с исходом.
pub(crate) fn expect_ready(outcome: FrameMaterializationOutcome) -> RenderableFrameKey {
    match outcome {
        FrameMaterializationOutcome::Ready(key) => key,
        other => panic!("frame must be Ready, got {other:?}"),
    }
}

/// Размер offscreen-цели стенда.
pub(crate) const HEADLESS_SIZE: [u32; 2] = [16, 8];

/// Сколько ждать GPU в тестах.
pub(crate) const GPU_TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Ресурс белого кадра.
pub(crate) const WHITE_FRAME: FrameResourceHandle = FrameResourceHandle(1);

/// Ресурс чёрного кадра.
pub(crate) const BLACK_FRAME: FrameResourceHandle = FrameResourceHandle(2);

/// Пиксели readback-а после BT.709 limited → RGB (допуск — [`PIXEL_TOLERANCE`]).
pub(crate) const WHITE_PIXEL: [u8; 4] = [255, 255, 255, 255];
pub(crate) const BLACK_PIXEL: [u8; 4] = [0, 0, 0, 255];

/// Допуск округления цветового преобразования шейдера.
pub(crate) const PIXEL_TOLERANCE: u8 = 4;

/// Размер HostPlanar-кадра стенда.
const FRAME_WIDTH: u32 = 4;
const FRAME_HEIGHT: u32 = 2;

/// BT.709 limited: Y = 235 — белый, Y = 16 — чёрный, U = V = 128 — без цвета.
const WHITE_LUMA: u8 = 235;
const BLACK_LUMA: u8 = 16;
const NEUTRAL_CHROMA: u8 = 128;

/// Что provider стенда отвечает на lookup дескриптора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SolidFrameLookup {
    Ready,
    Busy,
    Missing,
    Fatal,
}

/// Provider стенда: один ответ на все lookup-ы и журнал освобождений.
struct SolidFrameProvider {
    lookup: Arc<Mutex<SolidFrameLookup>>,
    ledger: ConformanceReleaseLedger,
}

impl SolidFrameProvider {
    fn current_lookup(&self) -> SolidFrameLookup {
        *self.lookup.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Плоскости Y/U/V одного цвета для кадра `handle`.
    fn descriptor(handle: FrameResourceHandle) -> HostPlanarFrameDescriptor {
        let luma = if handle == WHITE_FRAME {
            WHITE_LUMA
        } else {
            BLACK_LUMA
        };
        let luma_len = (FRAME_WIDTH * FRAME_HEIGHT) as usize;
        let chroma_len = luma_len / 4;
        let mut bytes = vec![luma; luma_len];
        bytes.extend(std::iter::repeat_n(NEUTRAL_CHROMA, chroma_len * 2));
        let plane = |role, offset, width: u32, height: u32| HostPlaneDescriptor {
            role,
            offset,
            stride: width as usize,
            visible_width: width,
            visible_height: height,
            bytes_per_sample: 1,
        };
        HostPlanarFrameDescriptor::from_owned_buffer(
            bytes,
            vec![
                plane(HostPlaneRole::Luma, 0, FRAME_WIDTH, FRAME_HEIGHT),
                plane(
                    HostPlaneRole::ChromaU,
                    luma_len,
                    FRAME_WIDTH / 2,
                    FRAME_HEIGHT / 2,
                ),
                plane(
                    HostPlaneRole::ChromaV,
                    luma_len + chroma_len,
                    FRAME_WIDTH / 2,
                    FRAME_HEIGHT / 2,
                ),
            ],
        )
    }
}

impl PresentFrameResourceProvider for SolidFrameProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        let resource_pool_lock_wait = Duration::ZERO;
        match self.current_lookup() {
            SolidFrameLookup::Ready => PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait,
            },
            SolidFrameLookup::Busy => PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait,
            },
            SolidFrameLookup::Missing => PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait,
            },
            SolidFrameLookup::Fatal => PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let resource_pool_lock_wait = Duration::ZERO;
        match self.current_lookup() {
            SolidFrameLookup::Ready => PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::HostPlanar(Self::descriptor(handle)),
                resource_pool_lock_wait,
            },
            SolidFrameLookup::Busy => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait,
            },
            SolidFrameLookup::Missing => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
            SolidFrameLookup::Fatal => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.ledger.record_release(handle);
    }
}

/// Поток «декодера» без декодирования: нужен только его provider.
struct SolidFrameDecoderThread {
    provider: PresentFrameResourceProviderHandle,
}

impl VideoDecoderThreadHandle for SolidFrameDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "solid-frame-test-decoder"
    }

    fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
        Ok(())
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.provider.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> Self::ResourceProvider {
        self.provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        None
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}

/// Запущенный «декодер» стенда и ручки управления им.
pub(crate) struct SolidFrameDecoder {
    /// Его получает `prepare_video_import`.
    pub(crate) backend: StartedVideoBackend,

    /// Журнал освобождений внутреннего provider-а.
    pub(crate) ledger: ConformanceReleaseLedger,

    /// Ответ provider-а на lookup (меняется тестом).
    lookup: Arc<Mutex<SolidFrameLookup>>,
}

impl SolidFrameDecoder {
    /// Запускает «декодер», который отвечает `Ready`.
    pub(crate) fn start() -> Self {
        let ledger = ConformanceReleaseLedger::default();
        let lookup = Arc::new(Mutex::new(SolidFrameLookup::Ready));
        let provider = PresentFrameResourceProviderHandle::new(SolidFrameProvider {
            lookup: lookup.clone(),
            ledger: ledger.clone(),
        });
        Self {
            backend: StartedVideoBackend::from_decoder_thread(
                "solid-frame-test-decoder",
                SolidFrameDecoderThread { provider },
            ),
            ledger,
            lookup,
        }
    }

    /// Ручка ответа provider-а; живёт дольше, чем `backend`, отданный рендеру.
    pub(crate) fn lookup_control(&self) -> Arc<Mutex<SolidFrameLookup>> {
        self.lookup.clone()
    }
}

/// Меняет ответ provider-а стенда.
pub(crate) fn set_lookup(control: &Arc<Mutex<SolidFrameLookup>>, next: SolidFrameLookup) {
    *control.lock().unwrap_or_else(PoisonError::into_inner) = next;
}

/// HostPlanar YUV420 8-бит кадр стенда.
pub(crate) fn host_planar_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
    decoded_frame(resource_handle, VideoFrameContract::host_yuv420_planar8())
}

/// Кадр с заданным контрактом и размером стенда.
pub(crate) fn decoded_frame(
    resource_handle: FrameResourceHandle,
    frame_contract: VideoFrameContract,
) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::from_millis(40),
        frame_contract,
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        render_width: FRAME_WIDTH,
        render_height: FRAME_HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Аренда кадра, как у плеера: освобождение уходит в provider обёрнутого декодера.
pub(crate) fn lease_for(
    frame: DecodedFrame,
    resource_provider: PresentFrameResourceProviderHandle,
) -> VideoFrameLease {
    VideoFrameLease::new(
        VideoFrameLeaseConfig::new(1, frame, Arc::new(ProviderReleaseSink))
            .with_resource_provider(resource_provider),
    )
}

/// Sink аренды: освобождение сразу уходит в provider аренды (обёртку рендера).
struct ProviderReleaseSink;

impl VideoFrameReleaseSink for ProviderReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        match release.resource_provider() {
            Some(provider) => {
                provider.release_frame(release.resource_handle());
                VideoFrameReleaseOutcome::Accepted
            }
            None => VideoFrameReleaseOutcome::NoOp,
        }
    }
}

/// Пиксели совпадают с допуском округления шейдера.
pub(crate) fn pixel_matches(actual: [u8; 4], expected: [u8; 4]) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.abs_diff(expected) <= PIXEL_TOLERANCE)
}

/// Читает один пиксель RGBA8 текстуры (копия в буфер → map → ожидание GPU).
pub(crate) fn read_texture_pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    [x, y]: [u32; 2],
) -> [u8; 4] {
    let bytes_per_row = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("render shell test readback"),
        size: u64::from(bytes_per_row),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render shell test readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("send readback result");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_TEST_TIMEOUT),
        })
        .expect("wait for GPU readback");
    receiver
        .recv_timeout(GPU_TEST_TIMEOUT)
        .expect("readback callback")
        .expect("map readback");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let pixel = [mapped[0], mapped[1], mapped[2], mapped[3]];
    drop(mapped);
    readback.unmap();
    pixel
}

/// Кадр offscreen-цели после readback-а: RGBA8 построчно, без выравнивания строк.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RgbaFrame {
    /// Ширина и высота в пикселях.
    pub(crate) size: [u32; 2],
    /// Пиксели построчно сверху вниз.
    pub(crate) pixels: Vec<[u8; 4]>,
}

impl RgbaFrame {
    /// Пиксели прямоугольника `[x0, y0]..[x1, y1)` построчно.
    pub(crate) fn region(&self, [x0, y0]: [u32; 2], [x1, y1]: [u32; 2]) -> Vec<[u8; 4]> {
        let width = self.size[0] as usize;
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (y as usize) * width + x as usize))
            .map(|index| self.pixels[index])
            .collect()
    }
}

/// Читает всю RGBA8-текстуру (копия в буфер с выравниванием строк → map → ожидание GPU).
pub(crate) fn read_texture_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> RgbaFrame {
    let [width, height] = [texture.width(), texture.height()];
    let unpadded_bytes_per_row = width * 4;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("render shell test frame readback"),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render shell test frame readback encoder"),
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
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("send frame readback result");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_TEST_TIMEOUT),
        })
        .expect("wait for GPU frame readback");
    receiver
        .recv_timeout(GPU_TEST_TIMEOUT)
        .expect("frame readback callback")
        .expect("map frame readback");
    let mapped = slice.get_mapped_range().expect("mapped frame range");
    let pixels = mapped
        .chunks_exact(padded_bytes_per_row as usize)
        .flat_map(|row| row[..unpadded_bytes_per_row as usize].as_chunks::<4>().0)
        .copied()
        .collect();
    drop(mapped);
    readback.unmap();
    RgbaFrame {
        size: [width, height],
        pixels,
    }
}
