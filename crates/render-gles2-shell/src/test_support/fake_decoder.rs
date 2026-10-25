//! Подставной программный декодер для тестов рендера: провайдер отдаёт заранее заданные
//! дескрипторы плоскостей и записывает возвраты кадров, поток декодера ничего не декодирует.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
    VideoDisplayOrientation,
};
use render_shell_api::conformance::ConformanceReleaseLedger;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup, StartedVideoBackend,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor, HostPlaneDescriptor,
    HostPlaneRole, VideoDecoderDiagnosticEvent, VideoDecoderThreadHandle, VideoFrameDiagnostics,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

/// Что провайдер отвечает на запрос дескриптора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LookupMode {
    Ready,
    Busy,
    Missing,
    Fatal,
}

/// Состояние провайдера: кадры, ответ, счётчики и журнал возвратов.
pub(crate) struct ProviderState {
    descriptors: Mutex<HashMap<u64, HostPlanarFrameDescriptor>>,
    mode: Mutex<LookupMode>,
    descriptor_lookups: AtomicUsize,
    released: Mutex<Vec<u64>>,
    /// Журнал conformance-набора контракта (если тест его ведёт).
    ledger: Option<ConformanceReleaseLedger>,
}

impl ProviderState {
    /// Кладёт кадр в «пул» провайдера.
    pub(crate) fn add_frame(&self, handle: u64, descriptor: HostPlanarFrameDescriptor) {
        self.descriptors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(handle, descriptor);
    }

    /// Меняет ответ на запросы дескриптора.
    pub(crate) fn set_mode(&self, mode: LookupMode) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    /// Сколько дескрипторов выдано.
    pub(crate) fn descriptor_lookups(&self) -> usize {
        self.descriptor_lookups.load(Ordering::SeqCst)
    }

    /// Сколько раз кадр `handle` вернулся провайдеру.
    pub(crate) fn release_count(&self, handle: u64) -> usize {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|released| **released == handle)
            .count()
    }
}

struct ScriptedProvider(Arc<ProviderState>);

impl PresentFrameResourceProvider for ScriptedProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.0.descriptor_lookups.fetch_add(1, Ordering::SeqCst);
        let resource_pool_lock_wait = Duration::ZERO;
        let mode = *self.0.mode.lock().unwrap_or_else(PoisonError::into_inner);
        match mode {
            LookupMode::Ready => {
                let descriptors = self
                    .0
                    .descriptors
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match descriptors.get(&handle.0) {
                    Some(descriptor) => PresentFrameResourceDescriptorLookup::Ready {
                        descriptor: FrameResourceDescriptor::HostPlanar(descriptor.clone()),
                        resource_pool_lock_wait,
                    },
                    None => PresentFrameResourceDescriptorLookup::Missing {
                        resource_pool_lock_wait,
                    },
                }
            }
            LookupMode::Busy => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait,
            },
            LookupMode::Missing => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
            LookupMode::Fatal => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.0
            .released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle.0);
        if let Some(ledger) = &self.0.ledger {
            ledger.record_release(handle);
        }
    }
}

/// Поток декодера без декодирования: носитель провайдера.
struct CarrierDecoder {
    provider: PresentFrameResourceProviderHandle,
}

impl VideoDecoderThreadHandle for CarrierDecoder {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "fake-software-decoder"
    }

    fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
        Err(DecodeSendError::Fatal(DecodeThreadError::new(
            "the fake decoder takes no packets",
        )))
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

/// Запущенный подставной декодер и ручки управления им.
pub(crate) struct FakeDecoder {
    pub(crate) backend: StartedVideoBackend,
    pub(crate) state: Arc<ProviderState>,
}

impl FakeDecoder {
    pub(crate) fn start() -> Self {
        Self::start_with_ledger(None)
    }

    /// Декодер, который дополнительно пишет возвраты кадров в журнал conformance-набора.
    pub(crate) fn start_with_ledger(ledger: Option<ConformanceReleaseLedger>) -> Self {
        let state = Arc::new(ProviderState {
            descriptors: Mutex::new(HashMap::new()),
            mode: Mutex::new(LookupMode::Ready),
            descriptor_lookups: AtomicUsize::new(0),
            released: Mutex::new(Vec::new()),
            ledger,
        });
        let provider =
            PresentFrameResourceProviderHandle::new(ScriptedProvider(Arc::clone(&state)));
        Self {
            backend: StartedVideoBackend::from_decoder_thread(
                "fake-software-decoder",
                CarrierDecoder { provider },
            ),
            state,
        }
    }
}

/// Раскладка кадра теста: левая и правая половины яркости, нейтральная цветность.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SplitPicture {
    /// Номер ресурса кадра в провайдере.
    pub(crate) handle: u64,
    /// Размер кадра (чётный по обеим осям).
    pub(crate) size: [u32; 2],
    /// Яркость левой половины (полный диапазон: серый цвет на экране тем же байтом).
    pub(crate) left_luma: u8,
    /// Яркость правой половины.
    pub(crate) right_luma: u8,
}

/// Параметры формата кадра «две половины яркости».
struct SplitFormat {
    pixel_layout: VideoFramePixelLayout,
    bytes_per_sample: usize,
    /// Код яркости левой и правой половины.
    luma_codes: [u16; 2],
    /// Код нейтральной цветности.
    neutral_chroma: u16,
    color: VideoColorMetadata,
}

/// Кадр 4:2:0 8 бит по раскладке и дескриптор его плоскостей (хвосты строк заполнены мусором).
pub(crate) fn split_picture_frame(
    picture: SplitPicture,
) -> (DecodedFrame, HostPlanarFrameDescriptor) {
    split_frame(
        picture.handle,
        picture.size,
        SplitFormat {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
            bytes_per_sample: 1,
            luma_codes: [u16::from(picture.left_luma), u16::from(picture.right_luma)],
            neutral_chroma: 128,
            color: VideoColorMetadata {
                range: ColorRange::Full,
                ..VideoColorMetadata::sdr_bt709_limited()
            },
        },
    )
}

/// HDR-кадр 4:2:0 10 бит (PQ, BT.2020, limited): левая половина светлая, правая тёмная.
/// Рендер показывает его через HDR → SDR (таблица строится на первом таком кадре).
pub(crate) fn hdr_pq_split_frame(
    handle: u64,
    size: [u32; 2],
) -> (DecodedFrame, HostPlanarFrameDescriptor) {
    split_frame(
        handle,
        size,
        SplitFormat {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
            bytes_per_sample: 2,
            luma_codes: [575, 300],
            neutral_chroma: 512,
            color: VideoColorMetadata {
                range: ColorRange::Limited,
                matrix: MatrixCoefficients::Bt2020,
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferFunction::Pq,
                ..VideoColorMetadata::sdr_bt709_limited()
            },
        },
    )
}

fn split_frame(
    handle: u64,
    size: [u32; 2],
    format: SplitFormat,
) -> (DecodedFrame, HostPlanarFrameDescriptor) {
    const ROW_TAIL_SAMPLES: usize = 6;
    const TAIL_FILL: u8 = 0xEE;
    let [width, height] = size;
    let sample_bytes = format.bytes_per_sample;
    let mut bytes = Vec::new();
    let mut planes = Vec::new();
    for (role, plane_width, plane_height) in [
        (HostPlaneRole::Luma, width, height),
        (HostPlaneRole::ChromaU, width / 2, height / 2),
        (HostPlaneRole::ChromaV, width / 2, height / 2),
    ] {
        let offset = bytes.len();
        for row in 0..plane_height {
            for x in 0..plane_width {
                let code = match role {
                    HostPlaneRole::Luma if x < plane_width / 2 => format.luma_codes[0],
                    HostPlaneRole::Luma => format.luma_codes[1],
                    HostPlaneRole::ChromaU | HostPlaneRole::ChromaV => format.neutral_chroma,
                };
                bytes.extend_from_slice(&code.to_le_bytes()[..sample_bytes]);
            }
            if row + 1 < plane_height {
                bytes.extend(std::iter::repeat_n(
                    TAIL_FILL,
                    ROW_TAIL_SAMPLES * sample_bytes,
                ));
            }
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride: (plane_width as usize + ROW_TAIL_SAMPLES) * sample_bytes,
            visible_width: plane_width,
            visible_height: plane_height,
            bytes_per_sample: sample_bytes,
        });
    }
    let frame = DecodedFrame {
        generation: 1,
        pts: Duration::from_millis(handle * 40),
        frame_contract: VideoFrameContract {
            pixel_layout: format.pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        width,
        height,
        render_width: width,
        render_height: height,
        display_orientation: VideoDisplayOrientation::Identity,
        color: format.color,
        resource_handle: FrameResourceHandle(handle),
        diagnostics: VideoFrameDiagnostics::default(),
    };
    (
        frame,
        HostPlanarFrameDescriptor::from_owned_buffer(bytes, planes),
    )
}

/// Возврат кадра из аренды идёт в провайдер аренды (обёртку рендера, которая ждёт fence) — как
/// у fallback-а плеера.
pub(crate) struct ProviderReleaseSink;

impl VideoFrameReleaseSink for ProviderReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        match release.resource_provider() {
            Some(provider) => {
                provider.release_frame(release.resource_handle());
                VideoFrameReleaseOutcome::Accepted
            }
            None => VideoFrameReleaseOutcome::NoOp,
        }
    }
}

/// Аренда кадра у «приложения» с провайдером декодера, который получил плеер.
pub(crate) fn lease_for(
    frame: DecodedFrame,
    provider: PresentFrameResourceProviderHandle,
) -> VideoFrameLease {
    VideoFrameLease::new(
        VideoFrameLeaseConfig::new(1, frame, Arc::new(ProviderReleaseSink))
            .with_resource_provider(provider),
    )
}
