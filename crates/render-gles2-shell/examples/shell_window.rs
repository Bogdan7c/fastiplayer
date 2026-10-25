//! Живая проверка GLES2-рендера плеера на настоящем окне (Wayland/X11): 780M и T410.
//!
//! Запуск (сценарий без клавиатуры, годится для ssh):
//! ```text
//! cargo run -p render-gles2-shell --example shell_window -- --scripted-check
//! ```
//! Сценарий: настоящий FFmpeg-декодер (VP9, фикстура `video-ffmpeg`) → контракт `RenderShell` →
//! окно; затем перенастройка показа на месте (`Immediate` / `Fifo` / `Mailbox`), отпуск и возврат
//! окна, смена размера, ожидание GPU. Печатает исходы и `scripted check: PASS`.
//!
//! Режим окна выбирает winit: нативный Wayland при `WAYLAND_DISPLAY`; XWayland — с пустым
//! `WAYLAND_DISPLAY` (`env -u WAYLAND_DISPLAY …`).

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use gles2_egui::WindowSource;
use media_core::{TimeBase, TrackId, TrackTimestamp};
use render_core::RenderViewport;
use render_gles2_shell::Gles2Renderer;
use render_shell_api::{
    DecoderSurfacePoolSlots, EguiFrameContent, FrameMaterializationOutcome,
    PresentationDetachOutcome, PresentationReattachOutcome, PresentationReconfigureError,
    PresentationReconfigured, RenderFrameDropReason, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor, RenderShell, RenderSurfaceSize, ShellPresentMode,
    SurfaceAlphaPreference, SurfaceFrameAcquire, SurfacePresentSettings, VideoImportPath,
    VideoImportPlan, WindowCornerMask,
};
use video_backend_api::VideoBackendDecoderThreadHandle;
use video_core::{
    DecodePacket, DecodedFrame, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const VP9_FIXTURE: &str =
    "../video-ffmpeg/tests/fixtures/direct-frame-buffers/vp9-176x100-12frames.ivf";
const VP9_TRACK_ID: TrackId = TrackId::new(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(200);
const DECODER_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Сколько повторять `materialize`, пока кольцо слотов занято.
const BUSY_RETRY_LIMIT: Duration = Duration::from_millis(500);

/// Возврат кадра из аренды идёт в провайдер аренды (обёртку рендера, ждущую fence).
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

/// Режет IVF на пакеты-кадры.
fn read_vp9_packets() -> Result<Vec<Bytes>, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(VP9_FIXTURE);
    let file = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut packets = Vec::new();
    let mut offset = 32;
    while offset + 12 <= file.len() {
        let length = u32::from_le_bytes(file[offset..offset + 4].try_into()?) as usize;
        packets.push(Bytes::copy_from_slice(
            &file[offset + 12..offset + 12 + length],
        ));
        offset += 12 + length;
    }
    Ok(packets)
}

/// Декодирует фикстуру и отдаёт каждый кадр `on_frame` сразу, как его выдал декодер: пул
/// декодера ограничен, кадры возвращаются по ходу (хвост DPB ждёт возврата показанных).
fn decode_fixture(
    decoder: &VideoBackendDecoderThreadHandle,
    mut on_frame: impl FnMut(DecodedFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    for (index, payload) in read_vp9_packets()?.into_iter().enumerate() {
        decoder
            .send_packet(DecodePacket {
                track_id: VP9_TRACK_ID,
                pts: FRAME_INTERVAL * u32::try_from(index)?,
                dts: None,
                track_pts: Some(TrackTimestamp::new(
                    VP9_TRACK_ID,
                    i64::try_from(index)?,
                    TimeBase::new(1, 5).ok_or("time base")?,
                )),
                track_dts: None,
                generation: 1,
                encoded_bytes: payload,
                keyframe: index == 0,
                resolved_color: None,
            })
            .map_err(|error| format!("send packet: {error:?}"))?;
        let deadline = Instant::now() + DECODER_TIMEOUT;
        while decoder.drain_completed_packet_count() == 0 {
            if let Some(error) = decoder.try_recv_error() {
                return Err(format!("decoder error: {error}").into());
            }
            if Instant::now() > deadline {
                return Err("decoder packet ACK timeout".into());
            }
            while let Some(frame) = decoder.try_recv_frame() {
                on_frame(frame)?;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        while let Some(frame) = decoder.try_recv_frame() {
            on_frame(frame)?;
        }
    }
    if !matches!(
        decoder.begin_end_of_stream_drain(1),
        VideoDecoderEndOfStreamDrainResult::Started(_)
    ) {
        return Err("end of stream drain did not start".into());
    }
    let deadline = Instant::now() + DECODER_TIMEOUT;
    loop {
        let drained = matches!(
            decoder.end_of_stream_drain_state(),
            VideoDecoderEndOfStreamDrainState::Drained { .. }
        );
        while let Some(frame) = decoder.try_recv_frame() {
            on_frame(frame)?;
        }
        if drained {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("drain timeout".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Показывает один кадр декодера в окне; ошибка — любой исход кроме `Presented`.
fn show_frame(
    renderer: &mut Gles2Renderer,
    decoder: &VideoBackendDecoderThreadHandle,
    frame: DecodedFrame,
    size: [u32; 2],
) -> Result<(), Box<dyn Error>> {
    let lease = VideoFrameLease::new(
        VideoFrameLeaseConfig::new(1, frame, Arc::new(ProviderReleaseSink))
            .with_resource_provider(decoder.resource_provider()),
    );
    // Busy — штатный ответ: все слоты кольца ещё ждут fence показа (vsync на живом окне). Плеер
    // на следующем такте пробует снова; сценарий делает то же с короткой паузой.
    let busy_deadline = Instant::now() + BUSY_RETRY_LIMIT;
    let key = loop {
        match renderer.video_frames().materialize(&lease).outcome {
            FrameMaterializationOutcome::Ready(key) => break key,
            FrameMaterializationOutcome::Busy if Instant::now() < busy_deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            other => return Err(format!("materialize: {other:?}").into()),
        }
    };
    let screen = RenderScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: 1.0,
    };
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        return Err("surface was not acquired".into());
    };
    let outcome = renderer.render_frame(RenderFrameInput {
        surface,
        video_frame: Some(&key),
        egui: EguiFrameContent::empty_fresh(),
        screen,
        video_viewport: RenderViewport::full_surface(size[0], size[1]),
        video_exclusion_rects: Vec::new(),
        window_corner_mask: WindowCornerMask::square(),
        subtitle_overlay: None,
    });
    match outcome {
        RenderFrameOutcome::Presented(_) => Ok(()),
        other => Err(format!("render_frame: {other:?}").into()),
    }
}

/// Подключает новый настоящий декодер к рендеру и проигрывает фикстуру целиком в окно.
fn play_clip(renderer: &mut Gles2Renderer, size: [u32; 2]) -> Result<usize, Box<dyn Error>> {
    let started = FfmpegSoftwareVideoBackendFactory::new().start_for_composition()?;
    let prepared = renderer
        .video_frames()
        .prepare_video_import(
            started,
            VideoImportPlan {
                path: VideoImportPath::SoftwareHostUpload,
                decoder_surface_pool: DecoderSurfacePoolSlots::new(16),
            },
        )
        .map_err(|error| format!("prepare_video_import: {error:?}"))?;
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .map_err(|error| format!("install_video_import: {error:?}"))?;
    let decoder = prepared.backend.into_decoder_thread();
    if decoder.configure_stream(VideoStreamDecodeConfig::from_requirement(
        VP9_TRACK_ID,
        &VideoDecodeRequirement::new(VideoCodec::Vp9),
        VideoFrameContract::host_yuv420_planar8(),
    )) != VideoStreamConfigResult::Configured
    {
        return Err("stream configuration was rejected".into());
    }
    let mut shown = 0;
    decode_fixture(decoder.as_ref(), |frame| {
        show_frame(renderer, decoder.as_ref(), frame, size)?;
        shown += 1;
        Ok(())
    })?;
    Ok(shown)
}

/// Сценарий проверки на окне `window`.
fn run_scripted_check(window: &Arc<Window>) -> Result<(), Box<dyn Error>> {
    let mut size = {
        let inner = window.inner_size();
        [inner.width, inner.height]
    };
    let source: Arc<dyn WindowSource> = window.clone();
    let mut renderer = Gles2Renderer::new(source, SurfacePresentSettings::default())?;
    println!("renderer: {}", renderer.capabilities().display_name);

    // 1. Видео из настоящего декодера в настоящем окне.
    let started_at = Instant::now();
    let shown = play_clip(&mut renderer, size)?;
    println!(
        "{shown} decoder frames presented, {:?} per frame",
        started_at.elapsed() / u32::try_from(shown.max(1))?
    );

    // 2. Перенастройка показа на месте.
    let base = SurfacePresentSettings::default();
    let requests = [
        ("immediate", ShellPresentMode::Immediate),
        ("fifo", ShellPresentMode::Fifo),
        ("mailbox", ShellPresentMode::Mailbox),
    ];
    for (name, present_mode) in requests {
        let settings = SurfacePresentSettings {
            present_mode,
            ..base
        };
        let outcome = renderer.reconfigure_presentation(settings)?;
        println!("reconfigure {name}: {outcome:?}");
        if present_mode == ShellPresentMode::Mailbox
            && outcome
                != (PresentationReconfigured::PresentModeFallback {
                    requested: present_mode,
                })
        {
            return Err("mailbox must be reported as a substitution".into());
        }
        play_clip(&mut renderer, size)?;
    }
    let repeated = renderer.reconfigure_presentation(SurfacePresentSettings {
        present_mode: ShellPresentMode::Mailbox,
        ..base
    });
    if repeated != Ok(PresentationReconfigured::AlreadyActive) {
        return Err(format!("repeating the settings: {repeated:?}").into());
    }
    let flipped = renderer.reconfigure_presentation(SurfacePresentSettings {
        alpha_preference: SurfaceAlphaPreference::TransparentPreferred,
        ..base
    });
    if flipped != Err(PresentationReconfigureError::AlphaPreferenceChangeUnsupported) {
        return Err(format!("alpha flip: {flipped:?}").into());
    }

    // 3. Отпуск и возврат окна («одно окно — одна surface»).
    if renderer.detach_presentation() != PresentationDetachOutcome::Detached {
        return Err("detach".into());
    }
    let screen = RenderScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: 1.0,
    };
    if !matches!(
        renderer.acquire_surface_frame(screen),
        SurfaceFrameAcquire::Dropped(RenderFrameDropReason::PresentationDetached)
    ) {
        return Err("a detached renderer must not give buffers".into());
    }
    renderer.wait_for_gpu_idle(IDLE_TIMEOUT)?;
    if renderer.reattach_presentation() != Ok(PresentationReattachOutcome::Reattached) {
        return Err("reattach".into());
    }
    play_clip(&mut renderer, size)?;
    println!("detach/reattach: the picture is back");

    // 4. Смена размера окна.
    let _ = window.request_inner_size(PhysicalSize::new(size[0] / 2, size[1] / 2));
    size = [size[0] / 2, size[1] / 2];
    renderer.resize(RenderSurfaceSize {
        width: size[0],
        height: size[1],
    });
    play_clip(&mut renderer, size)?;
    println!("resize to {}x{}: presented", size[0], size[1]);

    // 5. GPU дочитал, кадры вернулись декодеру, закрытие без падения.
    renderer.wait_for_gpu_idle(IDLE_TIMEOUT)?;
    println!("release stats: {:?}", renderer.frame_release_stats());
    println!("counters: {:?}", renderer.frame_counters());
    drop(renderer);
    Ok(())
}

struct ShellWindowApp {
    window: Option<Arc<Window>>,
    failure: Option<String>,
}

impl ApplicationHandler for ShellWindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("render-gles2-shell shell_window")
            .with_inner_size(LogicalSize::new(800.0, 450.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("window creation failed: {error}"));
                event_loop.exit();
                return;
            }
        };
        match run_scripted_check(&window) {
            Ok(()) => println!("scripted check: PASS"),
            Err(error) => self.failure = Some(format!("scripted check: FAIL: {error}")),
        }
        self.window = Some(window);
        event_loop.exit();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    if !std::env::args()
        .skip(1)
        .any(|argument| argument == "--scripted-check")
    {
        return Err("only --scripted-check is supported".into());
    }
    let mut app = ShellWindowApp {
        window: None,
        failure: None,
    };
    EventLoop::new()?.run_app(&mut app)?;
    match app.failure {
        Some(failure) => Err(failure.into()),
        None => Ok(()),
    }
}
