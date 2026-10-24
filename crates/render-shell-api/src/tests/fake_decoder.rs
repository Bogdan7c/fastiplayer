//! Fake-декодер для тестов контракта: один «готовый» кадр и журнал освобождений.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use video_backend_api::{
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup, StartedVideoBackend,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceHandle, VideoDecoderDiagnosticEvent, VideoDecoderThreadHandle,
    VideoFrameDiagnostics,
};
use video_frame_contract::VideoFrameContract;

use crate::conformance::ConformanceReleaseLedger;

/// Сколько «ждал» fake provider lock пула: проверяем, что рендер не теряет это значение.
pub(super) const FAKE_POOL_LOCK_WAIT: Duration = Duration::from_micros(7);

/// Что отвечает fake provider на lookup ресурса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeProviderLookup {
    Ready,
    Busy,
    Missing,
    Fatal,
}

/// Provider декодера: отвечает настроенным lookup-ом и записывает освобождения.
struct FakeDecoderProvider {
    lookup: Arc<Mutex<FakeProviderLookup>>,
    ledger: ConformanceReleaseLedger,
}

impl PresentFrameResourceProvider for FakeDecoderProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        let lookup = *self.lookup.lock().unwrap_or_else(PoisonError::into_inner);
        let resource_pool_lock_wait = FAKE_POOL_LOCK_WAIT;
        match lookup {
            FakeProviderLookup::Ready => PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait,
            },
            FakeProviderLookup::Busy => PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait,
            },
            FakeProviderLookup::Missing => PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait,
            },
            FakeProviderLookup::Fatal => PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.ledger.record_release(handle);
    }
}

/// Поток декодера без декодирования: нужен только его provider.
struct FakeDecoderThread {
    provider: PresentFrameResourceProviderHandle,
}

impl VideoDecoderThreadHandle for FakeDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "fake-decoder"
    }

    fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
        Ok(())
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.provider.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> Self::ResourceProvider {
        self.provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        None
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}

/// Запущенный fake-декодер и ручки управления им из теста.
pub(super) struct FakeDecoder {
    pub(super) backend: StartedVideoBackend,
    pub(super) ledger: ConformanceReleaseLedger,
    pub(super) lookup: Arc<Mutex<FakeProviderLookup>>,
}

impl FakeDecoder {
    pub(super) fn start() -> Self {
        let ledger = ConformanceReleaseLedger::default();
        let lookup = Arc::new(Mutex::new(FakeProviderLookup::Ready));
        let provider = PresentFrameResourceProviderHandle::new(FakeDecoderProvider {
            lookup: lookup.clone(),
            ledger: ledger.clone(),
        });
        Self {
            backend: StartedVideoBackend::from_decoder_thread(
                "fake-decoder",
                FakeDecoderThread { provider },
            ),
            ledger,
            lookup,
        }
    }
}

/// Меняет ответ provider-а (для проверки различимости Busy/Missing/Fatal).
pub(super) fn set_lookup(lookup: &Arc<Mutex<FakeProviderLookup>>, next: FakeProviderLookup) {
    *lookup.lock().unwrap_or_else(PoisonError::into_inner) = next;
}

/// Кадр декодера с заданным контрактом и ресурсом.
pub(super) fn decoded_frame(
    resource_handle: FrameResourceHandle,
    frame_contract: VideoFrameContract,
) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::from_millis(40),
        frame_contract,
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}
