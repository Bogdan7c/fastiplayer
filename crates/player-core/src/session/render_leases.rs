use crate::WgpuRenderTextureProviderHandle;
use crate::pipeline::{RenderLeaseReleaseEffect, VideoTextureReleaseEffect};

use super::PlayerSession;

/// Данные present frame, которые worker превращает в render lease без доступа к pipeline.
pub(crate) struct LeasedPresentFrame {
    /// Поколение render resources, в котором был создан frame/texture handle.
    pub render_generation: u64,

    /// Декодированный кадр, выбранный scheduler-ом для presentation.
    pub frame: video_core::DecodedFrame,

    /// `true`, если кадр ещё относится к старой позиции во время seek/scrub.
    pub stale: bool,

    /// WGPU provider texture views из backend-а, создавшего кадр.
    pub texture_provider: WgpuRenderTextureProviderHandle,
}

impl PlayerSession {
    /// Резервирует текущий present frame для render thread без раскрытия `PlaybackPipeline`.
    #[must_use]
    pub(crate) fn lease_present_video_frame(&mut self) -> Option<LeasedPresentFrame> {
        let texture_provider = self.pipeline.video_decoder_texture_view_provider()?;
        let frame = self.pipeline.present_video_frame()?.clone();
        let render_generation = self.pipeline.render_generation();
        let stale = self.snapshot.timeline.stale_frame;

        if !self.register_render_lease(render_generation, frame.texture_handle) {
            return None;
        }

        Some(LeasedPresentFrame {
            render_generation,
            frame,
            stale,
            texture_provider,
        })
    }

    /// Возвращает количество активных render leases без доступа тестов к pipeline fields.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn render_lease_count(&self) -> usize {
        self.pipeline.active_render_lease_count()
    }

    /// Проверяет, что release texture handle отложен до drop render lease.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_video_texture_release(
        &self,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        self.pipeline
            .has_deferred_video_texture_release(texture_handle)
    }

    /// Возвращает количество отложенных texture releases без раскрытия HashSet.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn deferred_video_texture_release_count(&self) -> usize {
        self.pipeline.deferred_render_release_count()
    }

    /// Регистрирует render lease для texture handle текущего поколения.
    pub(crate) fn register_render_lease(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        self.pipeline
            .try_register_render_lease(render_generation, texture_handle)
    }

    /// Снимает render lease и применяет отложенный texture release, если он уже был запрошен.
    #[cfg(test)]
    pub(crate) fn release_render_lease(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) {
        self.release_render_lease_with_provider(render_generation, texture_handle, None);
    }

    /// Снимает render lease и релизит texture через provider поколения, создавшего кадр.
    pub(crate) fn release_render_lease_with_provider(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
        texture_provider: Option<&WgpuRenderTextureProviderHandle>,
    ) {
        match self
            .pipeline
            .release_render_lease_accounting(render_generation, texture_handle)
        {
            RenderLeaseReleaseEffect::UnknownLease
            | RenderLeaseReleaseEffect::LeaseStillActive
            | RenderLeaseReleaseEffect::ReleasedWithoutDeferredTexture => {}
            RenderLeaseReleaseEffect::DeferredTextureReady => {
                self.release_deferred_video_texture(
                    render_generation,
                    texture_handle,
                    texture_provider,
                );
            }
        }
    }

    /// Освобождает texture handle сразу или откладывает release до завершения render lease.
    pub(crate) fn release_video_texture(&mut self, texture_handle: video_core::FrameTextureHandle) {
        match self.pipeline.request_video_texture_release(texture_handle) {
            VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop => {}
            VideoTextureReleaseEffect::ReleaseNow => self.release_video_texture_now(texture_handle),
        }
    }

    /// Выполняет deferred release тем способом, который соответствует поколению frame-а.
    fn release_deferred_video_texture(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
        texture_provider: Option<&WgpuRenderTextureProviderHandle>,
    ) {
        if let Some(texture_provider) = texture_provider {
            texture_provider.release_frame(texture_handle);
        } else if render_generation == self.pipeline.render_generation() {
            self.release_video_texture_now(texture_handle);
        }
    }

    /// Непосредственно отдаёт texture slot обратно decoder thread.
    fn release_video_texture_now(&mut self, texture_handle: video_core::FrameTextureHandle) {
        self.pipeline.release_frame_to_video_decoder(texture_handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_render_lease_rejects_stale_generation_without_accounting() {
        let mut session = PlayerSession::new();
        let stale_generation = session.pipeline.render_generation().saturating_add(1);
        let texture_handle = video_core::FrameTextureHandle(41);

        assert!(!session.register_render_lease(stale_generation, texture_handle));

        assert_eq!(session.render_lease_count(), 0);
        assert_eq!(session.deferred_video_texture_release_count(), 0);
    }

    #[test]
    fn deferred_texture_release_waits_until_last_render_lease() {
        let mut session = PlayerSession::new();
        let render_generation = session.pipeline.render_generation();
        let texture_handle = video_core::FrameTextureHandle(42);

        assert!(session.register_render_lease(render_generation, texture_handle));
        assert!(session.register_render_lease(render_generation, texture_handle));
        session.release_video_texture(texture_handle);

        session.release_render_lease(render_generation, texture_handle);

        assert_eq!(session.render_lease_count(), 1);
        assert!(session.has_deferred_video_texture_release(texture_handle));

        session.release_render_lease(render_generation, texture_handle);

        assert_eq!(session.render_lease_count(), 0);
        assert_eq!(session.deferred_video_texture_release_count(), 0);
    }

    #[test]
    fn unknown_render_lease_release_does_not_clear_deferred_release() {
        let mut session = PlayerSession::new();
        let render_generation = session.pipeline.render_generation();
        let leased_handle = video_core::FrameTextureHandle(43);
        let unknown_handle = video_core::FrameTextureHandle(44);

        assert!(session.register_render_lease(render_generation, leased_handle));
        session.release_video_texture(leased_handle);

        session.release_render_lease(render_generation, unknown_handle);

        assert_eq!(session.render_lease_count(), 1);
        assert!(session.has_deferred_video_texture_release(leased_handle));
    }
}
