//! Conformance-набор контракта `RenderShell` на НАСТОЯЩЕМ GLES2-рендере без окна.
//!
//! Тот же экзамен, что проходит wgpu-рендер: чужой билет, повтор UI, `Unsupported` ≠ `Error`,
//! «декодер не подключён», возврат кадра ровно один раз, потеря устройства, подготовщик в другом
//! потоке, отпуск и возврат окна, перенастройка показа, текстуры UI в отклонённом кадре.

use render_shell_api::conformance::{
    ConformanceReleaseLedger, ConformanceVideoSource, RenderShellConformanceHarness,
    run_render_shell_conformance,
};
use render_shell_api::{
    RenderScreenDescriptor, RenderShell, RenderSurfaceSize, RendererCreateError,
    SurfacePresentSettings,
};
use video_core::DecodedFrame;
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

use super::{SOFTWARE_PLAN, offscreen_or_skip, screen_for};
use crate::Gles2Renderer;
use crate::test_support::fake_decoder::{FakeDecoder, SplitPicture, split_picture_frame};

/// Размер цели стенда (2:1, как кадр стенда).
const TARGET_SIZE: [u32; 2] = [64, 32];

/// Номер ресурса готового кадра стенда.
const SOURCE_FRAME_HANDLE: u64 = 1;

struct Gles2ConformanceHarness;

impl RenderShellConformanceHarness for Gles2ConformanceHarness {
    fn create_renderer(&mut self) -> Result<Box<dyn RenderShell>, RendererCreateError> {
        Gles2Renderer::new_offscreen(
            RenderSurfaceSize {
                width: TARGET_SIZE[0],
                height: TARGET_SIZE[1],
            },
            self.initial_presentation(),
        )
        .map(|renderer| Box::new(renderer) as Box<dyn RenderShell>)
    }

    fn screen(&self) -> RenderScreenDescriptor {
        screen_for(TARGET_SIZE)
    }

    fn initial_presentation(&self) -> SurfacePresentSettings {
        SurfacePresentSettings::default()
    }

    /// Контракт, которого GLES2 не объявляет ни на каком драйвере: программный RGBA.
    fn unsupported_frame(&mut self) -> DecodedFrame {
        let (mut frame, _) = split_picture_frame(source_picture());
        frame.frame_contract = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Rgba8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        };
        frame
    }

    fn supported_video_source(&mut self) -> ConformanceVideoSource {
        let ledger = ConformanceReleaseLedger::default();
        let decoder = FakeDecoder::start_with_ledger(Some(ledger.clone()));
        let (frame, descriptor) = split_picture_frame(source_picture());
        decoder.state.add_frame(SOURCE_FRAME_HANDLE, descriptor);
        ConformanceVideoSource {
            backend: decoder.backend,
            plan: SOFTWARE_PLAN,
            frame,
            ledger,
        }
    }
}

fn source_picture() -> SplitPicture {
    SplitPicture {
        handle: SOURCE_FRAME_HANDLE,
        size: TARGET_SIZE,
        left_luma: 180,
        right_luma: 60,
    }
}

/// Все 11 проверок проходят на настоящем GLES2 без окна.
#[test]
fn the_gles2_renderer_passes_every_conformance_check() {
    if offscreen_or_skip(
        "the_gles2_renderer_passes_every_conformance_check",
        TARGET_SIZE,
    )
    .is_none()
    {
        return;
    }

    let violations = run_render_shell_conformance(&mut Gles2ConformanceHarness);

    assert!(
        violations.is_empty(),
        "GLES2 renderer violates the RenderShell contract: {violations:#?}"
    );
}
