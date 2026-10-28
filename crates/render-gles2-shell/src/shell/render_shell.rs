//! `render_shell_api::RenderShell` для GLES2-рендера: перевод вызовов контракта в операции
//! рендера. Логика кадра — `frame.rs`, склад кадров — `video_frames.rs`.

use std::time::Duration;

use gles2_egui::{PixelExtent, UiScreen};
use render_core::{
    RenderCapabilities, RenderDiagnostics, RenderLiveSettings, RenderLiveSettingsAdapter,
};
use render_shell_api::{
    FollowUpFrame, PresentationDetachOutcome, PresentationReattachError,
    PresentationReattachOutcome, PresentationReconfigureError, PresentationReconfigured,
    RenderFrameInput, RenderFrameOutcome, RenderHotPathStats, RenderScreenDescriptor, RenderShell,
    RenderSurfaceSize, RendererFamily, RendererGpuDrainError, RendererInstanceId,
    SurfaceFrameAcquire, SurfacePresentSettings, VideoFrameImport, VideoImportPreparer,
};

use super::Gles2Renderer;
use super::gpu_idle::wait_for_gpu_idle;
use crate::Gles2VideoImportPreparer;

impl RenderShell for Gles2Renderer {
    fn family(&self) -> RendererFamily {
        RendererFamily::Gles2
    }

    fn instance_id(&self) -> RendererInstanceId {
        self.instance_id
    }

    fn capabilities(&self) -> &RenderCapabilities {
        &self.capabilities
    }

    /// Нейтральная диагностика последнего кадра с видео (сбрасывается кадром без видео).
    fn diagnostics(&self) -> RenderDiagnostics {
        self.diagnostics.clone()
    }

    /// Размер цели; нулевой (свёрнутое окно) игнорируется, как у wgpu: кадр сам приведёт цель к
    /// своему размеру в `acquire_surface_frame`.
    fn resize(&mut self, size: RenderSurfaceSize) {
        if size.width == 0 || size.height == 0 {
            log::debug!("GLES2: resize в нулевой размер пропущен");
            return;
        }
        let extent = PixelExtent {
            width: size.width,
            height: size.height,
        };
        if let Err(error) = self.context.resize(extent) {
            log::warn!("GLES2: цель не приведена к размеру {extent:?}: {error}");
        }
    }

    fn reconfigure_presentation(
        &mut self,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        self.presentation.reconfigure(&mut self.context, settings)
    }

    fn detach_presentation(&mut self) -> PresentationDetachOutcome {
        self.forget_presented_state();
        self.presentation.detach(&mut self.context)
    }

    fn reattach_presentation(
        &mut self,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        self.forget_presented_state();
        self.presentation.reattach(&mut self.context)
    }

    fn acquire_surface_frame(&mut self, screen: RenderScreenDescriptor) -> SurfaceFrameAcquire {
        self.acquire_target_frame(screen)
    }

    /// `true`, если интерфейс можно повторить удержанными примитивами для этого экрана.
    fn can_reuse_retained_ui(&self, screen: RenderScreenDescriptor) -> bool {
        self.layers.as_ref().is_some_and(|layers| {
            layers.compositor.can_reuse_retained_ui(UiScreen {
                size_in_pixels: screen.size_in_pixels,
                pixels_per_point: screen.pixels_per_point,
            })
        })
    }

    fn render_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome {
        self.record_and_present_frame(input)
    }

    fn live_settings(&self) -> RenderLiveSettings {
        self.live_settings.settings().clone()
    }

    fn live_settings_adapter(&mut self) -> &mut dyn RenderLiveSettingsAdapter {
        &mut self.live_settings
    }

    fn wait_for_gpu_idle(&mut self, timeout: Duration) -> Result<(), RendererGpuDrainError> {
        let window_attached = !self.presentation.is_detached(&self.context);
        wait_for_gpu_idle(
            &mut self.context,
            self.release.binding(),
            window_attached,
            timeout,
        )
    }

    /// У GLES2 нет bind group-ов: счётчики горячего пути не ведутся (`results/08.md`).
    fn hot_path_stats(&self) -> RenderHotPathStats {
        RenderHotPathStats::default()
    }

    /// Видеослой досчитывает таблицу HDR → SDR в фоне (решение владельца 38): пока она не
    /// готова, на экране старая, а готовность замечает только следующий `draw`.
    fn needs_follow_up_frame(&self) -> FollowUpFrame {
        match &self.layers {
            Some(layers) if layers.video.has_pending_hdr_table() => FollowUpFrame::Soon,
            Some(_) | None => FollowUpFrame::NotNeeded,
        }
    }

    fn video_frames(&mut self) -> &mut dyn VideoFrameImport {
        self
    }

    fn video_import_preparer(&self) -> Box<dyn VideoImportPreparer> {
        Box::new(Gles2VideoImportPreparer::new(self.import_environment()))
    }
    fn present_pass_signal(&self) -> Option<video_core::PresentPassSignal> {
        Some(self.present_pass_signal_source())
    }
}
