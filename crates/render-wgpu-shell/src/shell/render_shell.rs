//! `render_shell_api::RenderShell` для wgpu-рендера (сессия 02 плана `user/gles2-renderer/`).
//!
//! Приложение держит `Box<dyn RenderShell>` и не видит ни wgpu, ни окна, ни способа импорта
//! кадров. Здесь только перевод вызовов контракта в прежние операции рендера; логика кадра —
//! в `shell.rs`, импорт кадров — в `crate::video_import`.

use std::time::Duration;

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
use render_wgpu_video::VideoHotPathStats;

use super::Renderer;

impl RenderShell for Renderer {
    fn family(&self) -> RendererFamily {
        RendererFamily::WgpuVulkan
    }

    fn instance_id(&self) -> RendererInstanceId {
        self.instance_id
    }

    fn capabilities(&self) -> &RenderCapabilities {
        self.video_renderer.capabilities()
    }

    /// Нейтральная диагностика последнего video pass (сбрасывается на кадре без видео).
    fn diagnostics(&self) -> RenderDiagnostics {
        self.video_renderer.diagnostics().clone()
    }

    /// Пересоздаёт конфигурацию цели показа при изменении размера окна.
    fn resize(&mut self, size: RenderSurfaceSize) {
        self.target
            .resize(&self.gpu.device, size.width, size.height);
    }

    /// Перенастройка present mode/задержки на текущей surface (сессия 02a): тот же device,
    /// импорт кадров и пайплайны не трогаются.
    fn reconfigure_presentation(
        &mut self,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        self.target.reconfigure_presentation(&self.gpu, settings)
    }

    /// Роняет surface окна (swapchain и `VkSurfaceKHR`): окно свободно для нового рендера.
    fn detach_presentation(&mut self) -> PresentationDetachOutcome {
        self.target.detach_presentation()
    }

    /// Новая surface тем же instance-ом на том же окне с прежней конфигурацией.
    fn reattach_presentation(
        &mut self,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        self.target.reattach_presentation(&self.gpu)
    }

    fn acquire_surface_frame(&mut self, screen: RenderScreenDescriptor) -> SurfaceFrameAcquire {
        self.acquire_target_frame(screen)
    }

    /// `true`, если интерфейс можно повторить прежними примитивами для этого размера экрана.
    /// После пересоздания рендера или смены размера ответ `false`.
    fn can_reuse_retained_ui(&self, screen: RenderScreenDescriptor) -> bool {
        self.egui_compositor
            .retained_frame_availability(&egui_wgpu::ScreenDescriptor {
                size_in_pixels: screen.size_in_pixels,
                pixels_per_point: screen.pixels_per_point,
            })
            .is_ok()
    }

    fn render_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome {
        self.record_and_present_frame(input)
    }

    /// Снимок живых настроек, который кандидат обязан сохранить при пересоздании.
    fn live_settings(&self) -> RenderLiveSettings {
        self.video_renderer.live_settings().clone()
    }

    fn live_settings_adapter(&mut self) -> &mut dyn RenderLiveSettingsAdapter {
        self
    }

    fn wait_for_gpu_idle(&mut self, timeout: Duration) -> Result<(), RendererGpuDrainError> {
        self.gpu.wait_for_gpu_idle(timeout)
    }

    /// Счётчики bind group и uniform video renderer-а (cpu-parity сессия 05); после
    /// пересоздания рендера начинаются с нуля.
    fn hot_path_stats(&self) -> RenderHotPathStats {
        neutral_hot_path_stats(self.video_renderer.hot_path_stats())
    }

    /// wgpu считает всё в кадре, где рисует: фоновой работы, меняющей картинку без новых
    /// входных данных, у него нет.
    fn needs_follow_up_frame(&self) -> FollowUpFrame {
        FollowUpFrame::NotNeeded
    }

    fn video_frames(&mut self) -> &mut dyn VideoFrameImport {
        &mut self.video_import
    }

    fn video_import_preparer(&self) -> Box<dyn VideoImportPreparer> {
        self.video_import.video_import_preparer()
    }

    /// wgpu сигнала не даёт: на GPU с Vulkan декод идёт своей очередью, и такт декодера
    /// по показу не нужен (решение владельца 77 — поведение wgpu-пути не меняется).
    fn present_pass_signal(&self) -> Option<video_core::PresentPassSignal> {
        None
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_texture_tests;

/// «Привязка ресурсов» контракта у wgpu — bind group кадра.
const fn neutral_hot_path_stats(stats: VideoHotPathStats) -> RenderHotPathStats {
    RenderHotPathStats {
        resource_bindings_created: stats.bind_groups_created,
        resource_bindings_reused: stats.bind_groups_reused,
        uniform_writes: stats.uniform_writes,
        uniform_writes_skipped: stats.uniform_writes_skipped,
    }
}
