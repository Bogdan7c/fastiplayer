use crate::PresentFrameResourceProviderHandle;
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

    /// Renderer-neutral provider resource-а из backend-а, создавшего кадр.
    pub resource_provider: PresentFrameResourceProviderHandle,
}

impl PlayerSession {
    /// Резервирует текущий present frame для render thread без раскрытия `PlaybackPipeline`.
    #[must_use]
    pub(crate) fn lease_present_video_frame(&mut self) -> Option<LeasedPresentFrame> {
        let resource_provider = self.pipeline.video_decoder_resource_provider()?;
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
            resource_provider,
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
        self.release_render_lease_with_provider(render_generation, texture_handle, None, false);
    }

    /// Снимает render lease и релизит texture через provider поколения, создавшего кадр.
    pub(crate) fn release_render_lease_with_provider(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
        resource_provider: Option<&PresentFrameResourceProviderHandle>,
        submitted_to_renderer: bool,
    ) {
        match self
            .pipeline
            .release_render_lease_accounting(render_generation, texture_handle)
        {
            RenderLeaseReleaseEffect::UnknownLease | RenderLeaseReleaseEffect::LeaseStillActive => {
            }
            RenderLeaseReleaseEffect::ReleasedWithoutDeferredTexture => {
                if submitted_to_renderer && let Some(resource_provider) = resource_provider {
                    self.pipeline.remember_rendered_texture_release_provider(
                        render_generation,
                        texture_handle,
                        resource_provider,
                    );
                }
            }
            RenderLeaseReleaseEffect::DeferredTextureReady => {
                self.release_deferred_video_texture(
                    render_generation,
                    texture_handle,
                    resource_provider,
                    submitted_to_renderer,
                );
            }
        }
    }

    /// Освобождает texture handle сразу или откладывает release до завершения render lease.
    pub(crate) fn release_video_texture(&mut self, texture_handle: video_core::FrameTextureHandle) {
        match self.pipeline.request_video_texture_release(texture_handle) {
            VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop => {}
            VideoTextureReleaseEffect::ReleaseViaRenderProvider(resource_provider) => {
                resource_provider.release_frame(texture_handle);
            }
            VideoTextureReleaseEffect::ReleaseNow => self.release_video_texture_now(texture_handle),
        }
    }

    /// Выполняет deferred release тем способом, который соответствует поколению frame-а.
    fn release_deferred_video_texture(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
        resource_provider: Option<&PresentFrameResourceProviderHandle>,
        submitted_to_renderer: bool,
    ) {
        if submitted_to_renderer && let Some(resource_provider) = resource_provider {
            resource_provider.release_frame(texture_handle);
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
    use std::sync::{Arc, Mutex};

    use crate::{PresentFrameResourceProvider, PresentFrameResourceProviderLookup};

    /// Fake provider, который показывает, каким release path-ом ушёл rendered frame.
    #[derive(Clone)]
    struct RecordingResourceProvider {
        /// Texture handles, освобождённые через renderer-owned provider.
        released_handles: Arc<Mutex<Vec<video_core::FrameTextureHandle>>>,
    }

    impl PresentFrameResourceProvider for RecordingResourceProvider {
        /// Lookup в этих тестах не используется: проверяется только release lifecycle.
        fn resource_lookup(
            &self,
            _handle: video_core::FrameTextureHandle,
        ) -> PresentFrameResourceProviderLookup {
            PresentFrameResourceProviderLookup::Ready {
                texture_pool_lock_wait: std::time::Duration::ZERO,
            }
        }

        /// Запоминает release, который должен пройти через renderer-owned boundary.
        fn release_frame(&self, handle: video_core::FrameTextureHandle) {
            self.released_handles
                .lock()
                .expect("recording provider release log lock")
                .push(handle);
        }
    }

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

    #[test]
    fn submitted_release_provider_survives_when_lease_drops_before_texture_release() {
        let mut session = PlayerSession::new();
        let render_generation = session.pipeline.render_generation();
        let texture_handle = video_core::FrameTextureHandle(45);
        let released_handles = Arc::new(Mutex::new(Vec::new()));
        let resource_provider =
            PresentFrameResourceProviderHandle::new(RecordingResourceProvider {
                released_handles: Arc::clone(&released_handles),
            });

        assert!(session.register_render_lease(render_generation, texture_handle));
        session.release_render_lease_with_provider(
            render_generation,
            texture_handle,
            Some(&resource_provider),
            true,
        );

        assert_eq!(session.render_lease_count(), 0);
        assert_eq!(
            released_handles
                .lock()
                .expect("recorded releases lock before player release")
                .as_slice(),
            &[]
        );

        session.release_video_texture(texture_handle);

        assert_eq!(
            released_handles
                .lock()
                .expect("recorded releases lock after player release")
                .as_slice(),
            &[texture_handle]
        );
    }

    #[test]
    fn unsubmitted_release_provider_is_not_used_for_later_texture_release() {
        let mut session = PlayerSession::new();
        let render_generation = session.pipeline.render_generation();
        let texture_handle = video_core::FrameTextureHandle(46);
        let released_handles = Arc::new(Mutex::new(Vec::new()));
        let resource_provider =
            PresentFrameResourceProviderHandle::new(RecordingResourceProvider {
                released_handles: Arc::clone(&released_handles),
            });

        assert!(session.register_render_lease(render_generation, texture_handle));
        session.release_render_lease_with_provider(
            render_generation,
            texture_handle,
            Some(&resource_provider),
            false,
        );

        session.release_video_texture(texture_handle);

        assert_eq!(
            released_handles
                .lock()
                .expect("recorded releases lock after unsubmitted player release")
                .as_slice(),
            &[]
        );
    }
}
