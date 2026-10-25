//! Тесты GLES2-рендера через публичный контракт `RenderShell` на настоящем EGL без окна.
//!
//! Без EGL тест пропускается с причиной в stderr; `GLES2_EGUI_REQUIRE_EGL=1` превращает
//! отсутствие EGL в падение (так в CI). Проверяются результаты — пиксели цели, исходы, возвраты
//! кадров декодеру, — а не вызовы.

mod conformance_run;
mod failed_frame_fence;
mod frame_flow;
mod hdr_follow_up;
mod key_identity;
mod lifecycle;
mod release_and_loss;
mod subtitle_support;
mod video_extras;

use std::sync::Arc;

use render_core::RenderViewport;
use render_shell_api::{
    DecoderSurfacePoolSlots, EguiFrameContent, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor, RenderShell, RenderSurfaceSize, RenderableFrameKey,
    RendererCreateStage, SubtitleOverlayFrame, SurfaceFrameAcquire, SurfacePresentSettings,
    VideoImportPath, VideoImportPlan, WindowCornerMask,
};
use video_backend_api::{PresentFrameResourceProviderHandle, VideoBackendDecoderThreadHandle};
use video_present_core::VideoFrameLease;

use super::Gles2Renderer;
use crate::test_support::fake_decoder::{
    FakeDecoder, ProviderState, SplitPicture, hdr_pq_split_frame, lease_for, split_picture_frame,
};

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

/// План программного импорта (пул декодера — число слотов не важно для загрузки).
pub(super) const SOFTWARE_PLAN: VideoImportPlan = VideoImportPlan {
    path: VideoImportPath::SoftwareHostUpload,
    decoder_surface_pool: DecoderSurfacePoolSlots::new(4),
};

/// Рендер без окна размера `size` или пропуск теста (нет EGL).
pub(super) fn offscreen_or_skip(test_name: &str, size: [u32; 2]) -> Option<Gles2Renderer> {
    let renderer = Gles2Renderer::new_offscreen(
        RenderSurfaceSize {
            width: size[0],
            height: size[1],
        },
        SurfacePresentSettings::default(),
    );
    match renderer {
        Ok(renderer) => Some(renderer),
        Err(error) if error.stage == RendererCreateStage::ApiConnection => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: renderer creation failed: {error}"),
    }
}

/// Экран кадра размера `size` в масштабе 1.
pub(super) fn screen_for(size: [u32; 2]) -> RenderScreenDescriptor {
    RenderScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: 1.0,
    }
}

/// Подключённый к рендеру подставной декодер: то, что видит плеер.
pub(super) struct ConnectedDecoder {
    /// Обёрнутый декодер плеера; держит провайдер, которым пользуются аренды.
    pub(super) player_thread: Box<VideoBackendDecoderThreadHandle>,
    /// Журнал и ответы внутреннего провайдера.
    pub(super) state: Arc<ProviderState>,
}

impl ConnectedDecoder {
    /// Провайдер, который получила аренда кадра.
    pub(super) fn lease_provider(&self) -> PresentFrameResourceProviderHandle {
        self.player_thread.resource_provider()
    }

    /// Аренда HDR-кадра 10 бит (PQ) размера `size`; кадр положен в пул провайдера.
    pub(super) fn lease_hdr(&self, handle: u64, size: [u32; 2]) -> VideoFrameLease {
        let (frame, descriptor) = hdr_pq_split_frame(handle, size);
        self.state.add_frame(handle, descriptor);
        lease_for(frame, self.lease_provider())
    }

    /// Аренда кадра картинки `picture` (кадр положен в пул провайдера).
    pub(super) fn lease(&self, picture: SplitPicture) -> VideoFrameLease {
        let (frame, descriptor) = split_picture_frame(picture);
        self.state.add_frame(picture.handle, descriptor);
        lease_for(frame, self.lease_provider())
    }
}

/// Подключает подставной программный декодер к рендеру: prepare → install.
pub(super) fn connect_software_decoder(renderer: &mut Gles2Renderer) -> ConnectedDecoder {
    let decoder = FakeDecoder::start();
    let prepared = renderer
        .video_frames()
        .prepare_video_import(decoder.backend, SOFTWARE_PLAN)
        .expect("software import is prepared");
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .expect("software import is installed");
    ConnectedDecoder {
        player_thread: prepared.backend.into_decoder_thread(),
        state: decoder.state,
    }
}

/// Готовит ключ кадра и вернуть вместе с арендой (аренда должна пережить ключ).
pub(super) fn materialize_key(
    renderer: &mut Gles2Renderer,
    decoder: &ConnectedDecoder,
    picture: SplitPicture,
) -> (RenderableFrameKey, VideoFrameLease) {
    let lease = decoder.lease(picture);
    let outcome = renderer.video_frames().materialize(&lease).outcome;
    match outcome {
        render_shell_api::FrameMaterializationOutcome::Ready(key) => (key, lease),
        other => panic!("frame {} must be Ready, got {other:?}", picture.handle),
    }
}

/// Что рисуется в кадре кроме видео.
#[derive(Default)]
pub(super) struct FrameExtras<'frame> {
    pub(super) viewport: Option<RenderViewport>,
    pub(super) exclusions: Vec<RenderViewport>,
    pub(super) subtitles: Option<SubtitleOverlayFrame<'frame>>,
    /// Интерфейс кадра; по умолчанию — пустой свежий проход.
    pub(super) egui: Option<EguiFrameContent>,
    /// Скругление углов окна; по умолчанию — квадратные.
    pub(super) corner_mask: Option<WindowCornerMask>,
}

/// Получает буфер и рисует кадр с пустым интерфейсом.
pub(super) fn present(
    renderer: &mut Gles2Renderer,
    size: [u32; 2],
    key: Option<&RenderableFrameKey>,
    extras: FrameExtras<'_>,
) -> RenderFrameOutcome {
    let screen = screen_for(size);
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        panic!("the surface is expected to be acquired");
    };
    renderer.render_frame(RenderFrameInput {
        surface,
        video_frame: key,
        egui: extras.egui.unwrap_or_else(EguiFrameContent::empty_fresh),
        screen,
        video_viewport: extras
            .viewport
            .unwrap_or_else(|| RenderViewport::full_surface(size[0], size[1])),
        video_exclusion_rects: extras.exclusions,
        window_corner_mask: extras.corner_mask.unwrap_or_else(WindowCornerMask::square),
        subtitle_overlay: extras.subtitles,
    })
}

/// Пиксель последнего кадра `[r, g, b, a]`.
pub(super) fn pixel(renderer: &Gles2Renderer, x: u32, y: u32) -> [u8; 4] {
    renderer
        .read_offscreen_pixels()
        .expect("offscreen readback")
        .pixel(x, y)
        .expect("pixel inside the target")
}

/// Яркость пикселя совпадает с ожидаемой с допуском округления шейдера.
pub(super) fn assert_gray(actual: [u8; 4], expected: u8, context: &str) {
    assert!(
        actual[..3]
            .iter()
            .all(|channel| channel.abs_diff(expected) <= 1)
            && actual[3] == 255,
        "{context}: expected gray {expected}, got {actual:?}"
    );
}
