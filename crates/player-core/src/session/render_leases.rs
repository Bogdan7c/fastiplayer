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

/// Стабильная identity текущего present frame без раскрытия `PlaybackPipeline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresentFrameIdentity {
    /// Поколение render resources, где был создан texture handle.
    render_generation: u64,

    /// Opaque texture handle decoded frame-а.
    texture_handle: video_core::FrameTextureHandle,
}

impl PresentFrameIdentity {
    /// Собирает identity из session-owned render generation и texture handle.
    #[must_use]
    pub(crate) const fn new(
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) -> Self {
        Self {
            render_generation,
            texture_handle,
        }
    }
}

impl PlayerSession {
    /// Возвращает identity текущего present frame для latest-slot render bridge-а.
    #[must_use]
    pub(crate) fn current_present_frame_identity(&self) -> Option<PresentFrameIdentity> {
        if !self.pipeline.has_active_video_decoder() {
            return None;
        }

        self.pipeline.present_video_frame().map(|frame| {
            PresentFrameIdentity::new(self.pipeline.render_generation(), frame.texture_handle)
        })
    }

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
    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use video_core::{DecodedPixelFormat, FrameMemoryPath, FrameTextureHandle};

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

    /// Минимальный decoder handle для проверки active-decoder guard-а render boundary.
    struct NoopVideoDecoderThread {
        /// Provider, который boundary должен возвращать clone-ом без владения backend state.
        resource_provider: PresentFrameResourceProviderHandle,
    }

    impl video_core::VideoDecoderThreadHandle for NoopVideoDecoderThread {
        type ResourceProvider = PresentFrameResourceProviderHandle;

        /// Возвращает стабильное имя fake backend-а для diagnostics.
        fn backend_name(&self) -> &'static str {
            "noop-render-lease-test"
        }

        /// Packet path в этих тестах не используется.
        fn send_packet(
            &self,
            _packet: video_core::DecodePacket,
        ) -> Result<(), video_core::DecodeSendError> {
            Ok(())
        }

        /// Release path в этих тестах не используется.
        fn release_frame(&self, _handle: FrameTextureHandle) {}

        /// Тест не публикует frames через decoder queue.
        fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
            None
        }

        /// Diagnostics stream для этого fake backend-а пустой.
        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            None
        }

        /// Fatal decoder errors в этом fake backend-е отсутствуют.
        fn try_recv_error(&self) -> Option<video_core::DecodeThreadError> {
            None
        }

        /// Flush является no-op, потому что decoder state отсутствует.
        fn flush(&self) -> anyhow::Result<()> {
            Ok(())
        }

        /// Возвращает renderer-neutral resource provider active decoder-а.
        fn resource_provider(&self) -> Self::ResourceProvider {
            self.resource_provider.clone()
        }

        /// Resource accounting в этих тестах не участвует.
        fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
            None
        }

        /// Packet queue fake backend-а всегда пуста.
        fn packet_queue_depth(&self) -> usize {
            0
        }

        /// Completed packet accounting fake backend-а всегда пустой.
        fn drain_completed_packet_count(&self) -> usize {
            0
        }
    }

    /// Создаёт decoded frame с renderer-neutral texture handle для boundary tests.
    fn decoded_frame_for_tests(texture_handle: FrameTextureHandle) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            generation: 0,
            pts: std::time::Duration::from_millis(42),
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Собирает fake decoder handle с renderer-neutral provider-ом.
    fn noop_video_decoder_thread() -> NoopVideoDecoderThread {
        let released_handles = Arc::new(Mutex::new(Vec::new()));
        let resource_provider =
            PresentFrameResourceProviderHandle::new(RecordingResourceProvider { released_handles });

        NoopVideoDecoderThread { resource_provider }
    }

    #[test]
    fn present_frame_identity_is_absent_without_active_decoder() {
        let mut session = PlayerSession::new();
        let texture_handle = FrameTextureHandle(47);

        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(texture_handle));

        assert_eq!(session.current_present_frame_identity(), None);
        assert_eq!(session.render_lease_count(), 0);
    }

    #[test]
    fn present_frame_identity_uses_active_decoder_without_registering_lease() {
        let mut session = PlayerSession::new();
        let texture_handle = FrameTextureHandle(48);

        session
            .pipeline
            .set_video_decoder_thread(noop_video_decoder_thread());
        let render_generation = session.pipeline.render_generation();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(texture_handle));

        let identity = session
            .current_present_frame_identity()
            .expect("active decoder and present frame should expose identity");

        assert_eq!(
            identity,
            PresentFrameIdentity::new(render_generation, texture_handle)
        );
        assert_eq!(session.render_lease_count(), 0);
    }

    #[test]
    fn register_render_lease_rejects_stale_generation_without_accounting() {
        let mut session = PlayerSession::new();
        let stale_generation = session.pipeline.render_generation().saturating_add(1);
        let texture_handle = FrameTextureHandle(41);

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
