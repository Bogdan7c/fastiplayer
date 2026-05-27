//! Нейтральный playback/backend API для video decoder backend-ов.
//!
//! Crate не знает о `player-core`, VA-API, WGPU или renderer materialization.
//! Он описывает только контракт, через который playback слой получает decoder
//! thread и renderer-neutral provider для lookup/release decoded resources.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

/// Decoder-thread handle, специализированный на renderer-neutral provider этого crate-а.
pub type VideoBackendDecoderThreadHandle =
    dyn video_core::VideoDecoderThreadHandle<ResourceProvider = PresentFrameResourceProviderHandle>;

/// Запущенный video backend, подготовленный composition layer-ом для playback pipeline.
pub struct StartedVideoBackend {
    /// Decoder thread остаётся за neutral handle boundary.
    decoder_thread: Box<VideoBackendDecoderThreadHandle>,
}

impl StartedVideoBackend {
    /// Создаёт backend wrapper вокруг decoder thread, который уже прошёл init handshake.
    #[must_use]
    pub fn from_decoder_thread(
        decoder_thread: impl video_core::VideoDecoderThreadHandle<
            ResourceProvider = PresentFrameResourceProviderHandle,
        > + 'static,
    ) -> Self {
        Self {
            decoder_thread: Box::new(decoder_thread),
        }
    }

    /// Передаёт decoder handle playback layer-у без раскрытия concrete backend type.
    #[must_use]
    pub fn into_decoder_thread(self) -> Box<VideoBackendDecoderThreadHandle> {
        self.decoder_thread
    }
}

/// Результат renderer-neutral lookup-а decoded resource-а без GPU handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentFrameResourceProviderLookup {
    /// Backend resource table доступен, opaque handle можно materialize в renderer layer-е.
    Ready {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend resource pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend обнаружил poisoned/fatal state при lookup-е.
    Error {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        texture_pool_lock_wait: Duration,
    },
}

impl PresentFrameResourceProviderLookup {
    /// Возвращает lock wait sample без раскрытия конкретного outcome.
    #[must_use]
    pub const fn texture_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                texture_pool_lock_wait,
                ..
            }
            | Self::Busy {
                texture_pool_lock_wait,
            }
            | Self::Missing {
                texture_pool_lock_wait,
            }
            | Self::Error {
                texture_pool_lock_wait,
            } => *texture_pool_lock_wait,
        }
    }
}

/// Renderer-neutral provider для status lookup-а и renderer-owned release.
pub trait PresentFrameResourceProvider: Send + Sync {
    /// Получает status и lock diagnostics для frame handle без возврата GPU handles.
    fn resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup;

    /// Пытается получить status без ожидания backend resource pool mutex-а.
    fn try_resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.resource_lookup(handle)
    }

    /// Освобождает renderer-owned frame после submitted GPU work или fallback release.
    fn release_frame(&self, handle: video_core::FrameTextureHandle);
}

/// Clone-able handle, который скрывает конкретный backend provider за trait boundary.
#[derive(Clone)]
pub struct PresentFrameResourceProviderHandle {
    /// Shared provider живёт столько же, сколько render leases, которые его держат.
    provider: Arc<dyn PresentFrameResourceProvider>,
}

impl PresentFrameResourceProviderHandle {
    /// Оборачивает concrete backend provider в renderer-neutral resource boundary handle.
    #[must_use]
    pub fn new(provider: impl PresentFrameResourceProvider + 'static) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Получает resource status и lock diagnostics через backend provider.
    #[must_use]
    pub fn resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.provider.resource_lookup(handle)
    }

    /// Пытается получить resource status без ожидания backend resource pool mutex-а.
    #[must_use]
    pub fn try_resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.provider.try_resource_lookup(handle)
    }

    /// Освобождает frame через backend provider, который создал texture handle.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.provider.release_frame(handle);
    }
}

/// Factory playback-facing video backend-а без привязки к concrete decoder crate-у.
pub trait VideoBackendFactory {
    /// Стартует backend и возвращает neutral decoder handle для playback pipeline-а.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend>;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Fake provider state, разделённый с тестом после передачи provider-а в handle.
    #[derive(Debug, Default)]
    struct RecordingProviderState {
        /// Handles, полученные через blocking resource lookup.
        resource_lookup_handles: Mutex<Vec<video_core::FrameTextureHandle>>,

        /// Handles, полученные через non-blocking resource lookup.
        try_resource_lookup_handles: Mutex<Vec<video_core::FrameTextureHandle>>,

        /// Handles, освобождённые через renderer-owned release path.
        released_handles: Mutex<Vec<video_core::FrameTextureHandle>>,
    }

    /// Fake resource provider, который записывает делегированные вызовы.
    struct RecordingResourceProvider {
        /// Shared state нужен тесту после move provider-а внутрь handle.
        state: Arc<RecordingProviderState>,
    }

    impl RecordingResourceProvider {
        /// Создаёт provider и возвращает shared state для assertions.
        fn new() -> (Self, Arc<RecordingProviderState>) {
            let state = Arc::new(RecordingProviderState::default());

            (
                Self {
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    impl PresentFrameResourceProvider for RecordingResourceProvider {
        fn resource_lookup(
            &self,
            handle: video_core::FrameTextureHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.state
                .resource_lookup_handles
                .lock()
                .expect("recording provider resource lookup state must be available")
                .push(handle);

            PresentFrameResourceProviderLookup::Ready {
                texture_pool_lock_wait: Duration::from_millis(7),
            }
        }

        fn try_resource_lookup(
            &self,
            handle: video_core::FrameTextureHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.state
                .try_resource_lookup_handles
                .lock()
                .expect("recording provider try lookup state must be available")
                .push(handle);

            PresentFrameResourceProviderLookup::Busy {
                texture_pool_lock_wait: Duration::from_millis(3),
            }
        }

        fn release_frame(&self, handle: video_core::FrameTextureHandle) {
            self.state
                .released_handles
                .lock()
                .expect("recording provider release state must be available")
                .push(handle);
        }
    }

    /// Minimal fake decoder для проверки startup wrapper-а без production backend resources.
    struct StartupFakeDecoderThread;

    impl video_core::VideoDecoderThreadHandle for StartupFakeDecoderThread {
        type ResourceProvider = PresentFrameResourceProviderHandle;

        fn backend_name(&self) -> &'static str {
            "startup fake decoder"
        }

        fn send_packet(
            &self,
            _packet: video_core::DecodePacket,
        ) -> Result<(), video_core::DecodeSendError> {
            Err(video_core::DecodeSendError::Fatal(
                video_core::DecodeThreadError::new("startup fake decoder does not accept packets"),
            ))
        }

        fn release_frame(&self, _handle: video_core::FrameTextureHandle) {}

        fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
            None
        }

        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<video_core::DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
            panic!("startup fake decoder does not provide renderer resources")
        }

        fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
            None
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            0
        }
    }

    /// Fake factory для success path neutral backend startup-а.
    struct SuccessfulVideoBackendFactory;

    impl VideoBackendFactory for SuccessfulVideoBackendFactory {
        fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
            Ok(StartedVideoBackend::from_decoder_thread(
                StartupFakeDecoderThread,
            ))
        }
    }

    /// Fake factory для typed error propagation без swallowing-а.
    struct FailingVideoBackendFactory;

    impl VideoBackendFactory for FailingVideoBackendFactory {
        fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
            Err(anyhow::anyhow!("fake backend startup failed"))
        }
    }

    /// Проверяет lock wait accessor для всех renderer-neutral lookup variants.
    #[test]
    fn texture_pool_lock_wait_returns_wait_for_all_lookup_variants() {
        let lookups = [
            PresentFrameResourceProviderLookup::Ready {
                texture_pool_lock_wait: Duration::from_millis(1),
            },
            PresentFrameResourceProviderLookup::Busy {
                texture_pool_lock_wait: Duration::from_millis(2),
            },
            PresentFrameResourceProviderLookup::Missing {
                texture_pool_lock_wait: Duration::from_millis(3),
            },
            PresentFrameResourceProviderLookup::Error {
                texture_pool_lock_wait: Duration::from_millis(4),
            },
        ];

        let waits: Vec<Duration> = lookups
            .iter()
            .map(PresentFrameResourceProviderLookup::texture_pool_lock_wait)
            .collect();

        assert_eq!(
            waits,
            vec![
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(3),
                Duration::from_millis(4),
            ]
        );
    }

    /// Проверяет, что handle не подменяет blocking resource lookup.
    #[test]
    fn present_frame_resource_provider_handle_delegates_resource_lookup() {
        let (provider, state) = RecordingResourceProvider::new();
        let handle = PresentFrameResourceProviderHandle::new(provider);
        let frame_handle = video_core::FrameTextureHandle(11);

        let lookup = handle.resource_lookup(frame_handle);

        assert_eq!(
            lookup,
            PresentFrameResourceProviderLookup::Ready {
                texture_pool_lock_wait: Duration::from_millis(7),
            }
        );
        assert_eq!(
            state
                .resource_lookup_handles
                .lock()
                .expect("resource lookup calls must be recorded")
                .as_slice(),
            [frame_handle]
        );
    }

    /// Проверяет, что handle сохраняет non-blocking lookup boundary.
    #[test]
    fn present_frame_resource_provider_handle_delegates_try_resource_lookup() {
        let (provider, state) = RecordingResourceProvider::new();
        let handle = PresentFrameResourceProviderHandle::new(provider);
        let frame_handle = video_core::FrameTextureHandle(17);

        let lookup = handle.try_resource_lookup(frame_handle);

        assert_eq!(
            lookup,
            PresentFrameResourceProviderLookup::Busy {
                texture_pool_lock_wait: Duration::from_millis(3),
            }
        );
        assert_eq!(
            state
                .try_resource_lookup_handles
                .lock()
                .expect("try lookup calls must be recorded")
                .as_slice(),
            [frame_handle]
        );
    }

    /// Проверяет, что release идёт через исходный provider.
    #[test]
    fn present_frame_resource_provider_handle_delegates_release_frame() {
        let (provider, state) = RecordingResourceProvider::new();
        let handle = PresentFrameResourceProviderHandle::new(provider);
        let frame_handle = video_core::FrameTextureHandle(23);

        handle.release_frame(frame_handle);

        assert_eq!(
            state
                .released_handles
                .lock()
                .expect("release calls must be recorded")
                .as_slice(),
            [frame_handle]
        );
    }

    /// Проверяет, что StartedVideoBackend отдаёт только neutral decoder handle.
    #[test]
    fn started_video_backend_returns_neutral_decoder_handle() {
        let started_backend = StartedVideoBackend::from_decoder_thread(StartupFakeDecoderThread);
        let decoder_thread = started_backend.into_decoder_thread();

        assert_eq!(decoder_thread.backend_name(), "startup fake decoder");
    }

    /// Проверяет success path factory без concrete backend dependency.
    #[test]
    fn fake_video_backend_factory_success_path_returns_backend() {
        let factory = SuccessfulVideoBackendFactory;
        let decoder_thread = factory
            .start_video_backend()
            .expect("fake backend startup must succeed")
            .into_decoder_thread();

        assert_eq!(decoder_thread.backend_name(), "startup fake decoder");
    }

    /// Проверяет, что factory error path возвращает ошибку caller-у.
    #[test]
    fn fake_video_backend_factory_error_path_returns_error() {
        let factory = FailingVideoBackendFactory;
        let error = match factory.start_video_backend() {
            Ok(_) => panic!("fake backend startup must fail"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "fake backend startup failed");
    }
}
