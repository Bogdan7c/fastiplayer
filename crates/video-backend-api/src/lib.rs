//! Нейтральный playback/backend API для video decoder backend-ов.
//!
//! Crate не знает о `player-core`, VA-API, WGPU или renderer materialization.
//! Он описывает только контракт, через который playback слой получает decoder
//! thread и renderer-neutral provider для lookup/release decoded resources.

#![forbid(unsafe_code)]

mod detached_backend;

use std::sync::Arc;
use std::time::Duration;

pub use detached_backend::{
    ConfiguredDetachedVideoBackend, DetachedVideoBackend,
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendConfigurationError, DetachedVideoBackendPortError,
    DetachedVideoBackendReply, DetachedVideoBackendRequest, DetachedVideoBackendResourceError,
    DetachedVideoBackendResourcePort, DetachedVideoBackendSelection,
};

/// Decoder-thread handle, специализированный на renderer-neutral provider этого crate-а.
pub type VideoBackendDecoderThreadHandle =
    dyn video_core::VideoDecoderThreadHandle<ResourceProvider = PresentFrameResourceProviderHandle>;

/// Shared renderer-neutral owner decoder thread-а.
pub type SharedVideoBackendDecoderThreadHandle = Arc<VideoBackendDecoderThreadHandle>;

/// Opaque keepalive, который renderer удерживает до завершения submitted resource release.
#[derive(Clone)]
pub struct VideoBackendLifetimeGuard {
    /// Concrete backend остаётся скрыт за neutral decoder boundary.
    _decoder_thread: SharedVideoBackendDecoderThreadHandle,
}

/// Делит decoder owner между playback handle и renderer completion callbacks.
#[must_use]
pub fn share_video_backend_decoder_thread(
    decoder_thread: Box<VideoBackendDecoderThreadHandle>,
) -> (
    SharedVideoBackendDecoderThreadHandle,
    VideoBackendLifetimeGuard,
) {
    let decoder_thread: SharedVideoBackendDecoderThreadHandle = Arc::from(decoder_thread);
    let lifetime_guard = VideoBackendLifetimeGuard {
        _decoder_thread: decoder_thread.clone(),
    };
    (decoder_thread, lifetime_guard)
}

/// Запущенный video backend, подготовленный composition layer-ом для playback pipeline.
pub struct StartedVideoBackend {
    /// Canonical backend id из capability report-а, например `vaapi` или `ffmpeg-sw`.
    backend_id: String,

    /// Decoder thread остаётся за neutral handle boundary.
    decoder_thread: Box<VideoBackendDecoderThreadHandle>,
}

impl StartedVideoBackend {
    /// Создаёт backend wrapper вокруг decoder thread, который уже прошёл init handshake.
    #[must_use]
    pub fn from_decoder_thread(
        backend_id: impl Into<String>,
        decoder_thread: impl video_core::VideoDecoderThreadHandle<
            ResourceProvider = PresentFrameResourceProviderHandle,
        > + 'static,
    ) -> Self {
        Self {
            backend_id: backend_id.into(),
            decoder_thread: Box::new(decoder_thread),
        }
    }

    /// Возвращает canonical backend id для связи active backend-а с capability output-ами.
    #[must_use]
    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    /// Передаёт decoder handle playback layer-у без раскрытия concrete backend type.
    #[must_use]
    pub fn into_decoder_thread(self) -> Box<VideoBackendDecoderThreadHandle> {
        self.decoder_thread
    }
}

/// Результат playback-facing lookup-а decoded resource-а без GPU handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentFrameResourceProviderLookup {
    /// Backend resource table доступен, opaque handle можно materialize в renderer layer-е.
    Ready {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Backend resource pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        resource_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Backend обнаружил poisoned/fatal state при lookup-е.
    Fatal {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },
}

impl PresentFrameResourceProviderLookup {
    /// Возвращает lock wait sample без раскрытия конкретного outcome.
    #[must_use]
    pub const fn resource_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                resource_pool_lock_wait,
                ..
            }
            | Self::Busy {
                resource_pool_lock_wait,
            }
            | Self::Missing {
                resource_pool_lock_wait,
            }
            | Self::Fatal {
                resource_pool_lock_wait,
            } => *resource_pool_lock_wait,
        }
    }
}

/// Результат renderer-facing descriptor lookup-а с duplicated platform handles.
#[derive(Debug)]
pub enum PresentFrameResourceDescriptorLookup {
    /// Backend вернул neutral descriptor; fd внутри descriptor-а принадлежат caller-у.
    Ready {
        /// Descriptor с duplicated owned platform handles.
        descriptor: video_core::FrameResourceDescriptor,

        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Backend resource pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        resource_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Backend не может безопасно дублировать descriptor.
    Fatal {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },
}

impl PresentFrameResourceDescriptorLookup {
    /// Возвращает lock wait sample без раскрытия concrete outcome.
    #[must_use]
    pub const fn resource_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                resource_pool_lock_wait,
                ..
            }
            | Self::Busy {
                resource_pool_lock_wait,
            }
            | Self::Missing {
                resource_pool_lock_wait,
            }
            | Self::Fatal {
                resource_pool_lock_wait,
            } => *resource_pool_lock_wait,
        }
    }
}

/// Renderer-neutral provider для status lookup-а и renderer-owned release.
pub trait PresentFrameResourceProvider: Send + Sync {
    /// Получает status и lock diagnostics для frame handle без возврата GPU handles.
    fn resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup;

    /// Пытается получить status без ожидания backend resource pool mutex-а.
    fn try_resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.resource_lookup(handle)
    }

    /// Получает duplicated resource descriptor для renderer-side materializer-а.
    fn resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        match self.resource_lookup(handle) {
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait,
            } => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait,
            } => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait,
            },
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait,
            } => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
            PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait,
            } => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    /// Пытается получить duplicated descriptor без ожидания backend mutex-а.
    fn try_resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.resource_descriptor_lookup(handle)
    }

    /// Освобождает renderer-owned frame после submitted GPU work или fallback release.
    fn release_frame(&self, handle: video_core::FrameResourceHandle);
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
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.provider.resource_lookup(handle)
    }

    /// Пытается получить resource status без ожидания backend resource pool mutex-а.
    #[must_use]
    pub fn try_resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.provider.try_resource_lookup(handle)
    }

    /// Получает duplicated descriptor через backend provider.
    #[must_use]
    pub fn resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.provider.resource_descriptor_lookup(handle)
    }

    /// Пытается получить duplicated descriptor без ожидания backend mutex-а.
    #[must_use]
    pub fn try_resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.provider.try_resource_descriptor_lookup(handle)
    }

    /// Освобождает frame через backend provider, который создал texture handle.
    pub fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        self.provider.release_frame(handle);
    }

    /// Проверяет, указывают ли два handle-а на один и тот же provider instance.
    ///
    /// Resource handle валиден только внутри своего provider-а, поэтому owner
    /// presentation path обязан сравнивать provider identity перед lookup-ом.
    #[must_use]
    pub fn same_provider(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.provider, &other.provider)
    }
}

/// Factory playback-facing video backend-а без привязки к concrete decoder crate-у.
pub trait VideoBackendFactory {
    /// Стартует backend и возвращает neutral decoder handle для playback pipeline-а.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend>;
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Fake provider state, разделённый с тестом после передачи provider-а в handle.
    #[derive(Debug, Default)]
    struct RecordingProviderState {
        /// Handles, полученные через blocking resource lookup.
        resource_lookup_handles: Mutex<Vec<video_core::FrameResourceHandle>>,

        /// Handles, полученные через non-blocking resource lookup.
        try_resource_lookup_handles: Mutex<Vec<video_core::FrameResourceHandle>>,

        /// Handles, полученные через blocking descriptor lookup.
        resource_descriptor_lookup_handles: Mutex<Vec<video_core::FrameResourceHandle>>,

        /// Handles, полученные через non-blocking descriptor lookup.
        try_resource_descriptor_lookup_handles: Mutex<Vec<video_core::FrameResourceHandle>>,

        /// Handles, освобождённые через renderer-owned release path.
        released_handles: Mutex<Vec<video_core::FrameResourceHandle>>,
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
            handle: video_core::FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.state
                .resource_lookup_handles
                .lock()
                .expect("recording provider resource lookup state must be available")
                .push(handle);

            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::from_millis(7),
            }
        }

        fn try_resource_lookup(
            &self,
            handle: video_core::FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.state
                .try_resource_lookup_handles
                .lock()
                .expect("recording provider try lookup state must be available")
                .push(handle);

            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: Duration::from_millis(3),
            }
        }

        fn resource_descriptor_lookup(
            &self,
            handle: video_core::FrameResourceHandle,
        ) -> PresentFrameResourceDescriptorLookup {
            self.state
                .resource_descriptor_lookup_handles
                .lock()
                .expect("recording provider descriptor lookup state must be available")
                .push(handle);

            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: sample_frame_resource_descriptor(31),
                resource_pool_lock_wait: Duration::from_millis(11),
            }
        }

        fn try_resource_descriptor_lookup(
            &self,
            handle: video_core::FrameResourceHandle,
        ) -> PresentFrameResourceDescriptorLookup {
            self.state
                .try_resource_descriptor_lookup_handles
                .lock()
                .expect("recording provider try descriptor lookup state must be available")
                .push(handle);

            PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait: Duration::from_millis(13),
            }
        }

        fn release_frame(&self, handle: video_core::FrameResourceHandle) {
            self.state
                .released_handles
                .lock()
                .expect("recording provider release state must be available")
                .push(handle);
        }
    }

    /// Создаёт owned fd для neutral descriptor tests.
    fn open_test_dma_buf_fd() -> OwnedFd {
        let file = File::open("/dev/null").expect("test fd source must be readable");
        file.into()
    }

    /// Создаёт minimal descriptor без WGPU/VAAPI/cros типов.
    fn sample_frame_resource_descriptor(resource_id: u64) -> video_core::FrameResourceDescriptor {
        video_core::FrameResourceDescriptor::DmaBuf(video_core::DmaBufFrameDescriptor {
            resource_id,
            fourcc: 0x3231_564e,
            export_layout: video_core::DmaBufFrameExportLayout::ComposedLayers,
            width: 1280,
            height: 720,
            objects: vec![video_core::DmaBufObjectDescriptor {
                fd: open_test_dma_buf_fd(),
                size: 4096,
                drm_format_modifier: 0,
                identity: video_core::DmaBufObjectIdentity {
                    device: 1,
                    inode: 2,
                    special_device: 3,
                },
            }],
            layers: vec![video_core::DmaBufLayerDescriptor {
                drm_format: 0x3231_564e,
                num_planes: 2,
                object_index: [0, 0, 0, 0],
                offset: [0, 2048, 0, 0],
                pitch: [1280, 1280, 0, 0],
            }],
        })
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

        fn release_frame(&self, _handle: video_core::FrameResourceHandle) {}

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
                "startup_fake",
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
    fn resource_pool_lock_wait_returns_wait_for_all_lookup_variants() {
        let lookups = [
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::from_millis(1),
            },
            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: Duration::from_millis(2),
            },
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait: Duration::from_millis(3),
            },
            PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: Duration::from_millis(4),
            },
        ];

        let waits: Vec<Duration> = lookups
            .iter()
            .map(PresentFrameResourceProviderLookup::resource_pool_lock_wait)
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

    /// Проверяет lock wait accessor для renderer descriptor lookup variants.
    #[test]
    fn descriptor_lookup_resource_pool_lock_wait_returns_wait_for_all_variants() {
        let lookups = [
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: sample_frame_resource_descriptor(1),
                resource_pool_lock_wait: Duration::from_millis(1),
            },
            PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait: Duration::from_millis(2),
            },
            PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait: Duration::from_millis(3),
            },
            PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait: Duration::from_millis(4),
            },
        ];

        let waits: Vec<Duration> = lookups
            .iter()
            .map(PresentFrameResourceDescriptorLookup::resource_pool_lock_wait)
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
        let frame_handle = video_core::FrameResourceHandle(11);

        let lookup = handle.resource_lookup(frame_handle);

        assert_eq!(
            lookup,
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::from_millis(7),
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
        let frame_handle = video_core::FrameResourceHandle(17);

        let lookup = handle.try_resource_lookup(frame_handle);

        assert_eq!(
            lookup,
            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: Duration::from_millis(3),
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

    /// Проверяет, что handle не синтезирует renderer descriptor вместо provider-а.
    #[test]
    fn present_frame_resource_provider_handle_delegates_resource_descriptor_lookup() {
        let (provider, state) = RecordingResourceProvider::new();
        let handle = PresentFrameResourceProviderHandle::new(provider);
        let frame_handle = video_core::FrameResourceHandle(19);

        let lookup = handle.resource_descriptor_lookup(frame_handle);

        match lookup {
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: video_core::FrameResourceDescriptor::DmaBuf(descriptor),
                resource_pool_lock_wait,
            } => {
                assert_eq!(descriptor.resource_id, 31);
                assert_eq!(descriptor.objects.len(), 1);
                assert_eq!(descriptor.layers.len(), 1);
                assert_eq!(resource_pool_lock_wait, Duration::from_millis(11));
            }
            _ => panic!("descriptor lookup must return provider-owned Ready descriptor"),
        }
        assert_eq!(
            state
                .resource_descriptor_lookup_handles
                .lock()
                .expect("descriptor lookup calls must be recorded")
                .as_slice(),
            [frame_handle]
        );
    }

    /// Проверяет, что non-blocking descriptor lookup сохраняет Fatal/Busy distinction.
    #[test]
    fn present_frame_resource_provider_handle_delegates_try_resource_descriptor_lookup() {
        let (provider, state) = RecordingResourceProvider::new();
        let handle = PresentFrameResourceProviderHandle::new(provider);
        let frame_handle = video_core::FrameResourceHandle(29);

        let lookup = handle.try_resource_descriptor_lookup(frame_handle);

        assert!(matches!(
            lookup,
            PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait
            } if resource_pool_lock_wait == Duration::from_millis(13)
        ));
        assert_eq!(
            state
                .try_resource_descriptor_lookup_handles
                .lock()
                .expect("try descriptor lookup calls must be recorded")
                .as_slice(),
            [frame_handle]
        );
    }

    /// Проверяет, что release идёт через исходный provider.
    #[test]
    fn present_frame_resource_provider_handle_delegates_release_frame() {
        let (provider, state) = RecordingResourceProvider::new();
        let handle = PresentFrameResourceProviderHandle::new(provider);
        let frame_handle = video_core::FrameResourceHandle(23);

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
        let started_backend =
            StartedVideoBackend::from_decoder_thread("startup_fake", StartupFakeDecoderThread);

        assert_eq!(started_backend.backend_id(), "startup_fake");

        let decoder_thread = started_backend.into_decoder_thread();

        assert_eq!(decoder_thread.backend_name(), "startup fake decoder");
    }

    #[test]
    fn renderer_lifetime_guard_keeps_shared_decoder_owner_alive() {
        let decoder_thread: Box<VideoBackendDecoderThreadHandle> =
            Box::new(StartupFakeDecoderThread);
        let (shared_decoder, lifetime_guard) = share_video_backend_decoder_thread(decoder_thread);

        assert_eq!(Arc::strong_count(&lifetime_guard._decoder_thread), 2);
        drop(shared_decoder);
        assert_eq!(Arc::strong_count(&lifetime_guard._decoder_thread), 1);
        assert_eq!(
            lifetime_guard._decoder_thread.backend_name(),
            "startup fake decoder"
        );
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
