//! Тесты контракта на fake-рендере.

mod catalog;
mod conformance_suite;
mod fake_decoder;
mod fake_shell;
mod video_import;

use video_core::FrameResourceHandle;
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use crate::conformance::{ConformanceVideoSource, RenderShellConformanceHarness};
use crate::{
    DecoderSurfacePoolSlots, RenderScreenDescriptor, RenderShell, RendererCreateError,
    RendererFamily, VideoImportPath, VideoImportPlan,
};

use self::fake_decoder::{FakeDecoder, decoded_frame};
use self::fake_shell::{FakeDefect, FakeRenderShell};

/// Ресурс «готового» кадра fake-декодера.
const SUPPORTED_FRAME_HANDLE: FrameResourceHandle = FrameResourceHandle(11);

/// Ресурс кадра, контракт которого fake не объявляет.
const UNSUPPORTED_FRAME_HANDLE: FrameResourceHandle = FrameResourceHandle(90);

/// Экран стенда.
const SCREEN: RenderScreenDescriptor = RenderScreenDescriptor {
    size_in_pixels: [800, 600],
    pixels_per_point: 1.0,
};

/// План импорта аппаратного декодера (пул на 4 поверхности).
const HARDWARE_PLAN: VideoImportPlan = VideoImportPlan {
    path: VideoImportPath::HardwareZeroCopy,
    decoder_surface_pool: DecoderSurfacePoolSlots::new(4),
};

/// Стенд conformance для fake-рендера с выбранным дефектом.
struct FakeHarness {
    defect: FakeDefect,
}

impl RenderShellConformanceHarness for FakeHarness {
    fn create_renderer(&mut self) -> Result<Box<dyn RenderShell>, RendererCreateError> {
        Ok(Box::new(FakeRenderShell::new(
            RendererFamily::Gles2,
            self.defect,
        )))
    }

    fn screen(&self) -> RenderScreenDescriptor {
        SCREEN
    }

    fn unsupported_frame(&mut self) -> video_core::DecodedFrame {
        decoded_frame(
            UNSUPPORTED_FRAME_HANDLE,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        )
    }

    fn supported_video_source(&mut self) -> ConformanceVideoSource {
        let decoder = FakeDecoder::start();
        ConformanceVideoSource {
            backend: decoder.backend,
            plan: HARDWARE_PLAN,
            frame: supported_frame(SUPPORTED_FRAME_HANDLE),
            ledger: decoder.ledger,
        }
    }
}

/// Кадр аппаратного декодера NV12, который fake объявляет.
fn supported_frame(handle: FrameResourceHandle) -> video_core::DecodedFrame {
    decoded_frame(
        handle,
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
    )
}
