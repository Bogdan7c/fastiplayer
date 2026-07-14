//! Focused Session 00C tests для bounded candidate ownership boundary.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use codec_core::{VideoCodec, VideoDisplayOrientation};
use media_core::TrackId;
use player_core::{MediaInstallRequestId, PlayerVideoDecoderThreadConfig};
use video_backend_api::{
    DetachedVideoBackend, DetachedVideoBackendCandidateCancellationCause,
    DetachedVideoBackendCandidateStatus, DetachedVideoBackendConfigurationError,
    DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendRequest,
    DetachedVideoBackendResourcePort, DetachedVideoBackendSelection, StartedVideoBackend,
};
use video_core::{DecodeThreadError, VideoStreamConfigResult, VideoStreamDecodeConfig};
use video_frame_contract::VideoFrameContract;

use super::resource_driver::{
    CandidateVideoBackendAvailability, CandidateVideoMaterializerKind,
    CandidateVideoPipelinePreparationError, CandidateVideoPipelinePreparationStage,
    PreparedCandidateVideoPipelineResources,
};
use super::*;

/// Fake renderer resource сообщает ID и считает exactly-once drop.
struct DropProbe {
    /// Stable ID отличает old active pointers от candidate pointers.
    id: u64,

    /// Shared counter принадлежит test fixture, а не resource owner-у.
    drop_count: Arc<AtomicUsize>,
}

impl DropProbe {
    /// Создаёт один owned renderer resource.
    fn new(id: u64, drop_count: Arc<AtomicUsize>) -> Self {
        // Constructor не изменяет accounting до фактического drop.
        Self { id, drop_count }
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        // Каждый unique owner увеличивает exact release counter один раз.
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

/// Minimal decoder fake позволяет player half пройти fallible configuration.
struct CandidateFakeDecoder {
    /// Configuration outcome выбирается deterministic test fixture-ом.
    configuration_result: VideoStreamConfigResult,

    /// Decoder owner release считается отдельно от app-half resources.
    drop_count: Arc<AtomicUsize>,
}

impl Drop for CandidateFakeDecoder {
    fn drop(&mut self) {
        // Detached/configured/started typestates всё равно владеют одним decoder-ом.
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

impl video_core::VideoDecoderThreadHandle for CandidateFakeDecoder {
    type ResourceProvider = video_backend_api::PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        // Stable fake name используется только diagnostics.
        "candidate fake decoder"
    }

    fn send_packet(
        &self,
        _packet: video_core::DecodePacket,
    ) -> Result<(), video_core::DecodeSendError> {
        // Session 00C candidate не принимает packets до ownership switch.
        Err(video_core::DecodeSendError::Fatal(DecodeThreadError::new(
            "candidate fake does not accept packets",
        )))
    }

    fn configure_stream(&self, _config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        // Точная configured/failure семантика возвращается без mutation active state.
        self.configuration_result.clone()
    }

    fn release_frame(&self, _handle: video_core::FrameResourceHandle) {
        // Fake не создаёт frames до candidate commit.
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        // Candidate configuration не публикует present frames.
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        // Focused fake не имеет дополнительной diagnostic queue.
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        // Configuration error возвращается синхронным typed result-ом.
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        // Fake flush не меняет ownership accounting.
        Ok(())
    }

    fn resource_provider(&self) -> video_backend_api::PresentFrameResourceProviderHandle {
        // Generic app tests не materialize-ят fake frames.
        panic!("candidate fake renderer provider must not be requested")
    }

    fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
        // Fake не резервирует surface pool.
        None
    }

    fn packet_queue_depth(&self) -> usize {
        // Packets до commit отсутствуют.
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        // Completion count остаётся нулевым без packets.
        0
    }
}

/// Driver behavior позволяет независимо fail-ить каждый preparation stage.
enum FakeDriverBehavior {
    /// Success создаёт exact matching decoder/materializer/binding pair.
    Success,

    /// Failure возвращает заранее выбранную typed причину без resource allocation.
    Failure(CandidateVideoPipelinePreparationError),
}

/// Fake driver доказывает pairing, admission и отсутствие destructive fallback.
struct FakeCandidateDriver {
    /// Current deterministic behavior одного driver invocation.
    behavior: FakeDriverBehavior,

    /// Число invocations обнаруживает второй startup и hidden retry.
    prepare_calls: usize,

    /// Число fallback invocations обязано оставаться нулём.
    destructive_fallback_calls: usize,

    /// Последний exact composition descriptor закрепляет VA-API/FFmpeg pairing.
    prepared_descriptors: Vec<CandidateVideoPipelineDescriptor>,

    /// Decoder half release counter.
    decoder_drop_count: Arc<AtomicUsize>,

    /// Materializer half release counter.
    materializer_drop_count: Arc<AtomicUsize>,

    /// Submission binding half release counter.
    binding_drop_count: Arc<AtomicUsize>,
}

impl FakeCandidateDriver {
    /// Создаёт successful bounded driver.
    fn successful() -> Self {
        // Каждый resource class получает независимый exactly-once counter.
        Self {
            behavior: FakeDriverBehavior::Success,
            prepare_calls: 0,
            destructive_fallback_calls: 0,
            prepared_descriptors: Vec::new(),
            decoder_drop_count: Arc::new(AtomicUsize::new(0)),
            materializer_drop_count: Arc::new(AtomicUsize::new(0)),
            binding_drop_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Создаёт driver с exact typed failure.
    fn failing(error: CandidateVideoPipelinePreparationError) -> Self {
        // Failure driver использует те же counters, которые обязаны остаться нулевыми.
        Self {
            behavior: FakeDriverBehavior::Failure(error),
            ..Self::successful()
        }
    }
}

impl CandidateVideoPipelineResourceDriver for FakeCandidateDriver {
    type Materializer = DropProbe;
    type SubmissionBinding = DropProbe;

    fn prepare_candidate_resources(
        &mut self,
        plan: VideoPipelinePlan,
    ) -> Result<
        PreparedCandidateVideoPipelineResources<Self::Materializer, Self::SubmissionBinding>,
        CandidateVideoPipelinePreparationError,
    > {
        // Invocation учитывается до outcome, поэтому retry/fallback виден тесту.
        self.prepare_calls += 1;
        // Exact expected pairing записывается для обоих production plan variants.
        let descriptor = CandidateVideoPipelineDescriptor::from_plan(plan);
        // Descriptor history bounded числом explicit test calls.
        self.prepared_descriptors.push(descriptor);

        // Typed failure не создаёт ни одной candidate half.
        if let FakeDriverBehavior::Failure(error) = &self.behavior {
            return Err(error.clone());
        }

        // Canonical fake backend ID обязан совпасть с выбранным plan path.
        let backend_id = match descriptor.backend_kind() {
            VideoBackendKind::HardwareZeroCopy => "vaapi",
            VideoBackendKind::FfmpegSoftware => video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID,
        };
        // Decoder wrapper моделирует отдельный started backend factory result.
        let started_backend = StartedVideoBackend::from_decoder_thread(
            backend_id,
            CandidateFakeDecoder {
                configuration_result: VideoStreamConfigResult::Configured,
                drop_count: self.decoder_drop_count.clone(),
            },
        );

        // Pair содержит ровно одну player half и две app-side owners.
        Ok(PreparedCandidateVideoPipelineResources {
            detached_backend: DetachedVideoBackend::from_started(started_backend),
            materializer: DropProbe::new(200, self.materializer_drop_count.clone()),
            submission_binding: DropProbe::new(300, self.binding_drop_count.clone()),
        })
    }
}

/// Player-half owner внутри fake port-а до cancel или Installed handoff.
enum FakePlayerHalf {
    /// Backend ещё не прошёл stream configuration.
    Detached(DetachedVideoBackend),

    /// Backend configured и готов к будущему player commit-у.
    Configured(video_backend_api::ConfiguredDetachedVideoBackend),
}

/// Fake port release-ит exact player half при app lifecycle cancellation.
struct FakeCandidatePort {
    /// Single player half соответствует bounded candidate slot-у.
    player_half: Option<FakePlayerHalf>,

    /// Disconnect возвращает typed error, но remote half всё равно закрывается.
    disconnected: bool,

    /// Cancel history содержит exact request/cause без coalescing.
    cancellations: Vec<(
        MediaInstallRequestId,
        DetachedVideoBackendCandidateCancellationCause,
    )>,
}

impl FakeCandidatePort {
    /// Создаёт connected port с detached player half.
    fn connected(detached_backend: DetachedVideoBackend) -> Self {
        // Port владеет ровно одной split half.
        Self {
            player_half: Some(FakePlayerHalf::Detached(detached_backend)),
            disconnected: false,
            cancellations: Vec::new(),
        }
    }

    /// Fallible настраивает player half и возвращает matching status.
    fn configure(
        &mut self,
        request_id: MediaInstallRequestId,
    ) -> DetachedVideoBackendCandidateStatus<MediaInstallRequestId> {
        // Detached owner забирается ровно один раз из bounded port slot-а.
        let detached_backend = match self.player_half.take() {
            Some(FakePlayerHalf::Detached(backend)) => backend,
            Some(FakePlayerHalf::Configured(backend)) => {
                // Возвращаем owner перед явным protocol panic в test fake-е.
                self.player_half = Some(FakePlayerHalf::Configured(backend));
                panic!("fake player half is already configured");
            }
            None => panic!("fake player half is absent"),
        };
        // Canonical ID читается до consuming configuration transition.
        let backend_id = detached_backend.backend_id().to_owned();
        // Focused fake всегда использует валидный selected stream config.
        match detached_backend.configure_stream(sample_stream_config()) {
            Ok(configured_backend) => {
                // Configured owner остаётся player-side до cancel/commit.
                self.player_half = Some(FakePlayerHalf::Configured(configured_backend));
                // App получает только matching typed status, не backend pointer.
                DetachedVideoBackendCandidateStatus::StreamConfigured {
                    request_id,
                    backend_id,
                }
            }
            Err(error) => {
                // Failed configuration уже освободила detached backend.
                DetachedVideoBackendCandidateStatus::ConfigurationFailed { request_id, error }
            }
        }
    }
}

impl DetachedVideoBackendResourcePort for FakeCandidatePort {
    type RequestId = MediaInstallRequestId;

    fn request_detached_backend(
        &mut self,
        _request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        // Slot test передаёт initial reply напрямую; повторный request запрещён.
        Err(DetachedVideoBackendPortError)
    }

    fn publish_candidate_status(
        &mut self,
        _status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        // Status direction вызывается test helper-ом напрямую в app slot.
        Err(DetachedVideoBackendPortError)
    }

    fn cancel_candidate(
        &mut self,
        request_id: Self::RequestId,
        cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        // Exact cancellation сохраняется до player-half release.
        self.cancellations.push((request_id, cause));
        // Disconnect/connected cancel оба закрывают remote owner exactly once.
        drop(self.player_half.take());
        // Caller различает successful dispatch и disconnected endpoint.
        if self.disconnected {
            Err(DetachedVideoBackendPortError)
        } else {
            Ok(())
        }
    }
}

/// Создаёт deterministic neutral media install request ID.
fn request_id(raw: u64) -> MediaInstallRequestId {
    // Tests передают только non-zero literals.
    MediaInstallRequestId::from_non_zero(NonZeroU64::new(raw).expect("request ID must be non-zero"))
}

/// Создаёт deterministic exact renderer generation.
fn renderer_generation(raw: u64) -> RendererGeneration {
    // Tests передают только non-zero generation literals.
    RendererGeneration::from_non_zero(
        NonZeroU64::new(raw).expect("renderer generation must be non-zero"),
    )
}

/// Строит VA-API production-shaped plan без запуска concrete backend-а.
fn vaapi_plan() -> VideoPipelinePlan {
    // Default decoder config содержит только bounded production limits.
    VideoPipelinePlan::VaapiDmaBufWgpu {
        decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
    }
}

/// Строит FFmpeg production-shaped plan без запуска concrete backend-а.
fn ffmpeg_plan() -> VideoPipelinePlan {
    // Тот же neutral config передаётся software factory path-у.
    VideoPipelinePlan::FfmpegHostUploadWgpu {
        decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
    }
}

/// Строит минимальный валидный software stream configuration.
fn sample_stream_config() -> VideoStreamDecodeConfig {
    // Contract совпадает с FFmpeg HostPlanar path, но fake backend принимает оба plans.
    VideoStreamDecodeConfig {
        track_id: TrackId::new(11),
        codec: VideoCodec::Vp9,
        profile: None,
        bit_depth: None,
        chroma: None,
        coded_width: Some(1280),
        coded_height: Some(720),
        display_orientation: VideoDisplayOrientation::Identity,
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
        codec_private: None,
        packetization: None,
    }
}

/// Забирает successful detached backend из exact-correlated reply.
fn available_backend(
    reply: DetachedVideoBackendReply<MediaInstallRequestId>,
    expected_request_id: MediaInstallRequestId,
) -> DetachedVideoBackend {
    // Reply identity проверяется до передачи player half fake port-у.
    let (actual_request_id, result) = reply.into_parts();
    // Mismatched reply нельзя принять даже при available backend.
    assert_eq!(actual_request_id, expected_request_id);
    // Focused success helper запрещает молча принять typed failure.
    result.unwrap_or_else(|error| panic!("candidate backend must be available: {error}"))
}
mod scenarios;
