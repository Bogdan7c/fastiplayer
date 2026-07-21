use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata, VideoProfile, Vp9Profile};
use media_core::{DemuxReadEvent, Packet, TrackKind, VideoTrackMetadata};
use video_backend_api::{
    DetachedVideoBackend, DetachedVideoBackendCandidateCancellationCause,
    DetachedVideoBackendCandidateStatus, DetachedVideoBackendPortError, DetachedVideoBackendReply,
    DetachedVideoBackendRequest, DetachedVideoBackendResourceError,
    DetachedVideoBackendResourcePort,
};

use super::super::PlayerSession;
use super::test_support::{
    FakeDemuxer, SharedFakeVideoDecoderThread, bt2020_pq_limited,
    capabilities_with_phase10_vp9_profile2_hdr, capabilities_with_vp9_profile0, fake_audio_packet,
    fake_track, install_fake_media, present_frame_for_current_seek_generation,
    test_decode_backend_id,
};
use crate::{
    AuthorizeInstallCommit, CancelMediaInstall, DecodeThreadError, MediaInstallCancellationCause,
    MediaInstallCompletion, MediaInstallControl, MediaInstallControlOutcome,
    MediaInstallFailureStage, MediaInstallReceipt, MediaInstallRequestId, MediaInstanceId,
    PlaybackIntent, PlaybackIntentRevision, PlaybackState, PlayerCommand, PlayerError,
    PlayerErrorKind, PlayerTickConfig, PreparedMedia, StartedVideoBackend, WorkerWakeupReason,
};

/// Shared observable state fake resource port-а после передачи его player owner-у.
#[derive(Default)]
struct FakeResourcePortState {
    /// Сколько exact backend requests сделал player candidate.
    request_count: usize,

    /// Все player statuses в порядке публикации.
    statuses: Vec<DetachedVideoBackendCandidateStatus<MediaInstallRequestId>>,
}

/// Deterministic Session 00C port без app/render concrete types.
struct FakeResourcePort {
    /// Единственный detached backend half для successful admission.
    detached_backend: Option<DetachedVideoBackend>,

    /// Typed resource failure вместо backend half-а.
    resource_error: Option<DetachedVideoBackendResourceError>,

    /// Optional wrong correlation identity для matching-failure tests.
    reply_request_id: Option<MediaInstallRequestId>,

    /// Имитирует disconnect app owner-а при player status publication.
    disconnect_on_status: bool,

    /// Shared counters/statuses для assertions после move в session.
    state: Arc<Mutex<FakeResourcePortState>>,
}

/// Создаёт VP9 candidate, который останавливает preflight на первом temporary event-е.
fn pending_vp9_prepared_media(label: &str) -> PreparedMedia {
    let mut video_track = fake_track(1, TrackKind::Video);
    video_track.video = Some(VideoTrackMetadata::empty());
    let retry_hint = media_core::DemuxRetryHint::new(Duration::from_millis(25))
        .expect("focused staged retry hint должен быть допустим");
    let mut demuxer = FakeDemuxer::new(
        vec![video_track],
        Some(Duration::from_secs(42)),
        Arc::new(Mutex::new(Vec::new())),
    );
    demuxer.push_event(DemuxReadEvent::TemporarilyUnavailable(retry_hint));
    PreparedMedia::from_external_label(label, Box::new(demuxer))
}

impl FakeResourcePort {
    /// Создаёт successful exact backend resource reply.
    fn available(
        decoder: SharedFakeVideoDecoderThread,
    ) -> (Self, Arc<Mutex<FakeResourcePortState>>) {
        let state = Arc::new(Mutex::new(FakeResourcePortState::default()));
        let started_backend =
            StartedVideoBackend::from_decoder_thread(test_decode_backend_id().as_str(), decoder);
        (
            Self {
                detached_backend: Some(DetachedVideoBackend::from_started(started_backend)),
                resource_error: None,
                reply_request_id: None,
                disconnect_on_status: false,
                state: Arc::clone(&state),
            },
            state,
        )
    }

    /// Создаёт typed resource failure до decoder configuration.
    fn unavailable(
        error: DetachedVideoBackendResourceError,
    ) -> (Self, Arc<Mutex<FakeResourcePortState>>) {
        let state = Arc::new(Mutex::new(FakeResourcePortState::default()));
        (
            Self {
                detached_backend: None,
                resource_error: Some(error),
                reply_request_id: None,
                disconnect_on_status: false,
                state: Arc::clone(&state),
            },
            state,
        )
    }

    /// Подменяет reply identity, не меняя request identity player-а.
    fn with_reply_request_id(mut self, reply_request_id: MediaInstallRequestId) -> Self {
        self.reply_request_id = Some(reply_request_id);
        self
    }

    /// Делает следующую status publication недоставляемой.
    fn with_status_disconnect(mut self) -> Self {
        self.disconnect_on_status = true;
        self
    }
}

impl DetachedVideoBackendResourcePort for FakeResourcePort {
    type RequestId = MediaInstallRequestId;

    /// Возвращает ровно один backend half либо scripted typed failure.
    fn request_detached_backend(
        &mut self,
        request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        self.state
            .lock()
            .expect("fake resource state lock")
            .request_count += 1;
        let request_id = self
            .reply_request_id
            .unwrap_or_else(|| *request.request_id());
        if let Some(error) = self.resource_error.take() {
            return Ok(DetachedVideoBackendReply::unavailable(request_id, error));
        }
        let Some(detached_backend) = self.detached_backend.take() else {
            return Ok(DetachedVideoBackendReply::unavailable(
                request_id,
                DetachedVideoBackendResourceError::Unavailable {
                    reason: "fake backend already consumed".to_owned(),
                },
            ));
        };
        Ok(DetachedVideoBackendReply::available(
            request_id,
            detached_backend,
        ))
    }

    /// Записывает lossless configured/failure/cancel status.
    fn publish_candidate_status(
        &mut self,
        status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        if self.disconnect_on_status {
            return Err(DetachedVideoBackendPortError);
        }
        self.state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .push(status);
        Ok(())
    }

    /// App-originated cancel в этих session tests не используется.
    fn cancel_candidate(
        &mut self,
        _request_id: Self::RequestId,
        _cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Ok(())
    }
}

/// Создаёт already-opened candidate media с выбранными track kinds.
fn prepared_media(track_kinds: &[TrackKind]) -> PreparedMedia {
    let tracks = track_kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let track_id = (index + 1) as u32;
            match kind {
                TrackKind::Video => staged_vp9_track(track_id),
                TrackKind::Audio => fake_track(track_id, TrackKind::Audio),
            }
        })
        .collect::<Vec<_>>();
    prepared_media_from_tracks(tracks)
}

/// Создаёт реалистичный VP9 SDR track с полным container evidence для protocol tests.
fn staged_vp9_track(track_id: u32) -> media_core::TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Video);
    let mut metadata = VideoTrackMetadata::empty();
    metadata.profile = Some(VideoProfile::Vp9(Vp9Profile::Profile0));
    metadata.bit_depth = Some(BitDepth::Eight);
    metadata.chroma = Some(ChromaSubsampling::Yuv420);
    metadata.coded_width = Some(1_920);
    metadata.coded_height = Some(1_080);
    metadata.color = Some(VideoColorMetadata::sdr_bt709_limited());
    track.video = Some(metadata);
    track
}

/// Создаёт VP9 Profile 2 keyframe с 10-bit 4:2:0 BT.2020 header-ом.
fn vp9_profile2_10bit_bt2020_keyframe(width: u32, height: u32) -> Bytes {
    let mut bits = Vec::new();
    push_bits(&mut bits, 0b10, 2);
    push_vp9_profile(&mut bits, 2);
    bits.push(0);
    bits.push(0);
    bits.push(1);
    bits.push(0);
    push_bits(&mut bits, 0x49_83_42, 24);
    bits.push(0);
    push_bits(&mut bits, 5, 3);
    bits.push(0);
    push_bits(&mut bits, width - 1, 16);
    push_bits(&mut bits, height - 1, 16);
    bits.push(0);
    Bytes::from(bits_to_bytes(&bits))
}

/// Кодирует VP9 profile bits из uncompressed header.
fn push_vp9_profile(bits: &mut Vec<u8>, profile: u8) {
    bits.push(profile & 1);
    bits.push((profile >> 1) & 1);
    if profile == 3 {
        bits.push(0);
    }
}

/// Добавляет старшие `width` битов числа в VP9 bitstream order.
fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
    for shift in (0..width).rev() {
        bits.push(((value >> shift) & 1) as u8);
    }
}

/// Упаковывает MSB-first bits в encoded bytes.
fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            let mut byte = 0u8;
            for (index, bit) in chunk.iter().enumerate() {
                byte |= bit << (7 - index);
            }
            byte
        })
        .collect()
}

/// Создаёт candidate media из exact scripted tracks.
fn prepared_media_from_tracks(tracks: Vec<media_core::TrackInfo>) -> PreparedMedia {
    let demuxer = FakeDemuxer::new(
        tracks,
        Some(Duration::from_secs(42)),
        Arc::new(Mutex::new(Vec::new())),
    );
    PreparedMedia::from_external_label("candidate", Box::new(demuxer))
}

/// Устанавливает observable old instance, который staged failures не имеют права менять.
fn install_old_media(
    session: &mut PlayerSession,
    playback_state: PlaybackState,
) -> MediaInstanceId {
    install_fake_media(session, vec![fake_track(90, TrackKind::Audio)]);
    let old_instance_id =
        MediaInstanceId::from_non_zero(NonZeroU64::new(90).expect("test instance id is non-zero"));
    session.snapshot.media_instance_id = Some(old_instance_id);
    session.set_playback_state(playback_state);
    old_instance_id
}

/// Strong install обязан уточнить VP9 HDR по packet header до backend request-а
/// и после commit-а вернуть все prefetched packets в исходном demux order.
#[test]
fn vp9_hdr_packet_preflight_selects_p010_and_replays_interleaved_audio() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Paused);

    let mut video_track = fake_track(1, TrackKind::Video);
    let mut video_metadata = VideoTrackMetadata::empty();
    video_metadata.coded_width = Some(3_840);
    video_metadata.coded_height = Some(2_160);
    video_metadata.color = Some(bt2020_pq_limited());
    video_track.video = Some(video_metadata);
    let audio_track = fake_track(2, TrackKind::Audio);

    let audio_packet = fake_audio_packet(
        crate::TrackId::new(2),
        Duration::ZERO,
        Duration::from_millis(20),
    );
    let encoded_video = vp9_profile2_10bit_bt2020_keyframe(3_840, 2_160);
    let video_packet = Packet::new(
        crate::TrackId::new(1),
        TrackKind::Video,
        Duration::ZERO,
        None,
        true,
        encoded_video,
    );
    let mut demuxer = FakeDemuxer::new(
        vec![video_track, audio_track],
        Some(Duration::from_secs(42)),
        Arc::new(Mutex::new(Vec::new())),
    );
    demuxer.push_event(DemuxReadEvent::Packet(audio_packet.clone()));
    demuxer.push_event(DemuxReadEvent::Packet(video_packet.clone()));

    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(100).expect("test request id is non-zero"),
    );
    let decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, resource_state) = FakeResourcePort::available(decoder.clone());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        PreparedMedia::from_external_label("vp9-hdr", Box::new(demuxer)),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );

    assert!(receipt.try_take_ready_to_commit().is_some());
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        1
    );
    let configured_streams = decoder.configured_streams();
    let configured_stream = configured_streams
        .last()
        .expect("detached decoder должен быть настроен до ReadyToCommit");
    assert_eq!(
        configured_stream.profile,
        Some(VideoProfile::Vp9(Vp9Profile::Profile2))
    );
    assert_eq!(configured_stream.bit_depth, Some(BitDepth::Ten));
    assert_eq!(configured_stream.chroma, Some(ChromaSubsampling::Yuv420));
    assert_eq!(
        configured_stream.frame_contract.pixel_layout,
        video_frame_contract::VideoFramePixelLayout::P010
    );

    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert_eq!(
        session
            .pipeline
            .demux_next_event()
            .expect("installed demuxer должен существовать")
            .expect("prefetched audio event должен читаться без ошибки"),
        DemuxReadEvent::Packet(audio_packet)
    );
    assert_eq!(
        session
            .pipeline
            .demux_next_event()
            .expect("installed demuxer должен существовать")
            .expect("prefetched video event должен читаться без ошибки"),
        DemuxReadEvent::Packet(video_packet)
    );
}

/// Temporary readiness обязан оставить request pre-Ready и продолжить тот же probe state.
#[test]
fn staged_video_preflight_resumes_without_replaying_temporary_readiness() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Paused);
    let mut video_track = fake_track(1, TrackKind::Video);
    let mut video_metadata = VideoTrackMetadata::empty();
    video_metadata.coded_width = Some(3_840);
    video_metadata.coded_height = Some(2_160);
    video_metadata.color = Some(bt2020_pq_limited());
    video_track.video = Some(video_metadata);
    let audio_track = fake_track(2, TrackKind::Audio);
    let audio_packet = fake_audio_packet(
        crate::TrackId::new(2),
        Duration::ZERO,
        Duration::from_millis(20),
    );
    let video_packet = Packet::new(
        crate::TrackId::new(1),
        TrackKind::Video,
        Duration::ZERO,
        None,
        true,
        vp9_profile2_10bit_bt2020_keyframe(3_840, 2_160),
    );
    let retry_hint = media_core::DemuxRetryHint::new(Duration::from_millis(25))
        .expect("focused staged retry hint должен быть допустим");
    let event_read_count = Arc::new(AtomicUsize::new(0));
    let mut demuxer = FakeDemuxer::new(
        vec![video_track, audio_track],
        Some(Duration::from_secs(42)),
        Arc::new(Mutex::new(Vec::new())),
    )
    .with_event_read_count(Arc::clone(&event_read_count));
    demuxer.push_event(DemuxReadEvent::TemporarilyUnavailable(retry_hint));
    demuxer.push_event(DemuxReadEvent::Packet(audio_packet.clone()));
    demuxer.push_event(DemuxReadEvent::Packet(video_packet.clone()));
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(101).expect("test request id is non-zero"),
    );
    let decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, resource_state) = FakeResourcePort::available(decoder);
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        PreparedMedia::from_external_label("resumable-vp9-hdr", Box::new(demuxer)),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );

    assert!(receipt.try_take_ready_to_commit().is_none());
    assert_eq!(event_read_count.load(Ordering::Relaxed), 1);
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::NotReady
    );
    assert!(session.has_staged_media_install());
    assert!(receipt.try_take_completion().is_none());
    let wakeup = session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(5),
        Duration::from_millis(250),
    );
    assert_eq!(wakeup.reason, WorkerWakeupReason::StagedPreflightDeadline);
    assert!(wakeup.delay.is_some_and(|delay| !delay.is_zero()));
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        0
    );
    session.service_pending_staged_preflight(Instant::now());
    assert_eq!(event_read_count.load(Ordering::Relaxed), 1);

    session.service_pending_staged_preflight(Instant::now() + Duration::from_millis(30));

    assert!(receipt.try_take_ready_to_commit().is_some());
    assert_eq!(event_read_count.load(Ordering::Relaxed), 3);
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        1
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert_eq!(
        session
            .pipeline
            .demux_next_event()
            .expect("installed demuxer должен существовать")
            .expect("prefetched audio event должен сохраниться"),
        DemuxReadEvent::Packet(audio_packet)
    );
    assert_eq!(
        session
            .pipeline
            .demux_next_event()
            .expect("installed demuxer должен существовать")
            .expect("prefetched video event должен сохраниться"),
        DemuxReadEvent::Packet(video_packet)
    );
}

/// Wall-clock timeout terminal-resolve-ит pending request до Ready barrier-а.
#[test]
fn staged_video_preflight_timeout_is_typed_and_exactly_once() {
    let mut session =
        PlayerSession::new().with_staged_video_preflight_timeout(Duration::from_millis(10));
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    let mut video_track = fake_track(1, TrackKind::Video);
    video_track.video = Some(VideoTrackMetadata::empty());
    let retry_hint = media_core::DemuxRetryHint::new(Duration::from_millis(25))
        .expect("focused staged retry hint должен быть допустим");
    let mut demuxer = FakeDemuxer::new(
        vec![video_track],
        Some(Duration::from_secs(42)),
        Arc::new(Mutex::new(Vec::new())),
    );
    demuxer.push_event(DemuxReadEvent::TemporarilyUnavailable(retry_hint));
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(102).expect("test request id is non-zero"),
    );
    let (resource_port, resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        PreparedMedia::from_external_label("timed-out-vp9", Box::new(demuxer)),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    session.service_pending_staged_preflight(Instant::now() + Duration::from_millis(15));

    assert!(receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::VideoPreflightTimeout
    ));
    assert!(receipt.try_take_completion().is_none());
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        0
    );
    assert!(!session.has_staged_media_install());
}

/// Pending preflight сохраняет exact cancellation cause при cancel, supersede и shutdown.
#[test]
fn pending_staged_preflight_terminal_resolves_each_request_exactly_once() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Paused);
    let request_ids = [103_u64, 104, 105].map(|raw_request_id| {
        MediaInstallRequestId::from_non_zero(
            NonZeroU64::new(raw_request_id).expect("test request id is non-zero"),
        )
    });
    let mut receipts = Vec::new();
    let mut resource_states = Vec::new();

    for (index, request_id) in request_ids.into_iter().enumerate() {
        let (resource_port, resource_state) =
            FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
        let (receipt, install_port) = MediaInstallReceipt::new(request_id);
        session.stage_prepared_media_install(
            request_id,
            pending_vp9_prepared_media(&format!("pending-vp9-{index}")),
            PlaybackIntent::StartPaused,
            PlaybackIntentRevision::INITIAL,
            install_port,
            Box::new(resource_port),
        );
        assert!(receipt.try_take_ready_to_commit().is_none());
        receipts.push(receipt);
        resource_states.push(resource_state);

        if index == 0 {
            assert_eq!(
                session.apply_staged_media_install_control(MediaInstallControl::Cancel(
                    CancelMediaInstall {
                        request_id,
                        cause: MediaInstallCancellationCause::UserCancelled,
                    },
                )),
                MediaInstallControlOutcome::CancellationAccepted
            );
        }
    }

    assert!(matches!(
        receipts[0].try_take_completion(),
        Some(MediaInstallCompletion::Cancelled {
            cause: MediaInstallCancellationCause::UserCancelled,
            ..
        })
    ));
    assert!(matches!(
        receipts[1].try_take_completion(),
        Some(MediaInstallCompletion::Cancelled {
            cause: MediaInstallCancellationCause::Superseded,
            ..
        })
    ));
    session
        .dispatch_command(PlayerCommand::Shutdown)
        .expect("shutdown должен terminal-resolve pending preflight");
    assert!(matches!(
        receipts[2].try_take_completion(),
        Some(MediaInstallCompletion::Cancelled {
            cause: MediaInstallCancellationCause::LifecycleShutdown,
            ..
        })
    ));
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.try_take_completion().is_none())
    );
    assert!(resource_states.iter().all(|resource_state| {
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count
            == 0
    }));
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert!(!session.has_staged_media_install());
}

#[test]
fn ready_to_commit_preserves_old_playing_until_atomic_authorization_switch() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);
    let old_render_generation = session.pipeline.render_generation();
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(101).expect("test request id is non-zero"),
    );
    let decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, resource_state) = FakeResourcePort::available(decoder.clone());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video, TrackKind::Audio]),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );

    assert!(session.has_staged_media_install());
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.pipeline.render_generation(), old_render_generation);
    assert!(receipt.try_take_ready_to_commit().is_some());
    assert!(receipt.try_take_completion().is_none());
    assert_eq!(decoder.configured_streams().len(), 1);
    assert!(matches!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .as_slice(),
        [DetachedVideoBackendCandidateStatus::StreamConfigured { request_id: status_request_id, .. }]
            if *status_request_id == request_id
    ));

    let outcome = session.apply_staged_media_install_control(MediaInstallControl::Authorize(
        AuthorizeInstallCommit { request_id },
    ));
    assert_eq!(outcome, MediaInstallControlOutcome::AuthorizationAccepted);
    let completion = receipt
        .try_take_completion()
        .expect("accepted authorization publishes Installed synchronously");
    let MediaInstallCompletion::Installed {
        request_id: installed_request_id,
        media_instance_id,
        ..
    } = completion
    else {
        panic!("accepted authorization must publish Installed");
    };
    assert_eq!(installed_request_id, request_id);
    assert_ne!(media_instance_id, old_instance_id);
    assert_eq!(
        session.snapshot().media_instance_id,
        Some(media_instance_id)
    );
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(
        session.pipeline.render_generation(),
        old_render_generation.wrapping_add(1)
    );
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(crate::TrackId::new(1))
    );
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(crate::TrackId::new(2))
    );
    assert!(!session.has_staged_media_install());
}

#[test]
fn cancel_while_ready_preserves_old_paused_and_duplicate_control_is_typed() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Paused);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(102).expect("test request id is non-zero"),
    );
    let (resource_port, resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(receipt.try_take_ready_to_commit().is_some());

    let cancellation = CancelMediaInstall {
        request_id,
        cause: MediaInstallCancellationCause::UserCancelled,
    };
    let outcome =
        session.apply_staged_media_install_control(MediaInstallControl::Cancel(cancellation));
    assert_eq!(outcome, MediaInstallControlOutcome::CancellationAccepted);
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Cancelled {
            request_id: completion_request_id,
            cause: MediaInstallCancellationCause::UserCancelled,
        }) if completion_request_id == request_id
    ));
    assert!(matches!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .last(),
        Some(DetachedVideoBackendCandidateStatus::Cancelled {
            request_id: status_request_id,
            cause: DetachedVideoBackendCandidateCancellationCause::Requested,
        }) if *status_request_id == request_id
    ));
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Cancel(cancellation)),
        MediaInstallControlOutcome::AlreadyTerminal
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit {
                request_id: MediaInstallRequestId::from_non_zero(
                    NonZeroU64::new(999).expect("test request id is non-zero"),
                ),
            },
        )),
        MediaInstallControlOutcome::StaleRequest
    );
}

#[test]
fn resource_and_configuration_failures_are_pre_ready_and_preserve_old_playing() {
    let mut resource_failure_session = PlayerSession::new();
    resource_failure_session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut resource_failure_session, PlaybackState::Playing);
    let resource_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(103).expect("test request id is non-zero"),
    );
    let (resource_port, _) =
        FakeResourcePort::unavailable(DetachedVideoBackendResourceError::StartupFailed {
            backend_id: test_decode_backend_id().as_str().to_owned(),
            message: "candidate decoder startup failed".to_owned(),
        });
    let (resource_receipt, install_port) = MediaInstallReceipt::new(resource_request_id);
    resource_failure_session.stage_prepared_media_install(
        resource_request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(resource_receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        resource_receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::CandidateVideoResourceAcquisition
    ));
    assert_eq!(
        resource_failure_session.snapshot().media_instance_id,
        Some(old_instance_id)
    );
    assert_eq!(
        resource_failure_session.playback_state(),
        PlaybackState::Playing
    );

    let mut configure_failure_session = PlayerSession::new();
    configure_failure_session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut configure_failure_session, PlaybackState::Playing);
    let configure_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(104).expect("test request id is non-zero"),
    );
    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.push_configure_result(video_core::VideoStreamConfigResult::Fatal(
        DecodeThreadError::new("candidate configure failed"),
    ));
    let (resource_port, resource_state) = FakeResourcePort::available(decoder);
    let (configure_receipt, install_port) = MediaInstallReceipt::new(configure_request_id);
    configure_failure_session.stage_prepared_media_install(
        configure_request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(configure_receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        configure_receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::CandidateVideoBackendConfiguration
    ));
    assert!(matches!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .as_slice(),
        [DetachedVideoBackendCandidateStatus::ConfigurationFailed { .. }]
    ));
    assert_eq!(
        configure_failure_session.snapshot().media_instance_id,
        Some(old_instance_id)
    );
    assert_eq!(
        configure_failure_session.playback_state(),
        PlaybackState::Playing
    );
}

#[test]
fn reply_matching_failure_never_crosses_ready_barrier() {
    // Wrong reply correlation отбрасывается до configure и сохраняет old Paused.
    let mut matching_session = PlayerSession::new();
    matching_session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut matching_session, PlaybackState::Paused);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(110).expect("test request id is non-zero"),
    );
    let wrong_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(111).expect("test request id is non-zero"),
    );
    let (resource_port, resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    matching_session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port.with_reply_request_id(wrong_request_id)),
    );
    assert!(receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::CandidateVideoBackendMatching
    ));
    assert!(matches!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .as_slice(),
        [DetachedVideoBackendCandidateStatus::Cancelled {
            request_id: cancelled_request_id,
            ..
        }] if *cancelled_request_id == request_id
    ));
    assert_eq!(
        matching_session.snapshot().media_instance_id,
        Some(old_instance_id)
    );
    assert_eq!(matching_session.playback_state(), PlaybackState::Paused);
}

#[test]
fn status_publication_failure_never_crosses_ready_barrier() {
    // Configured candidate без доставленного status также остаётся pre-barrier failure.
    let mut status_session = PlayerSession::new();
    status_session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut status_session, PlaybackState::Playing);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(112).expect("test request id is non-zero"),
    );
    let (resource_port, _) = FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    status_session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port.with_status_disconnect()),
    );
    assert!(receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::CandidateVideoStatusPublication
    ));
    assert_eq!(
        status_session.snapshot().media_instance_id,
        Some(old_instance_id)
    );
    assert_eq!(status_session.playback_state(), PlaybackState::Playing);
}

#[test]
fn pure_track_reselection_failure_never_requests_candidate_resources() {
    // Unsupported track отбрасывается pure planner-ом до resource request-а.
    let mut planning_session = PlayerSession::new();
    let old_instance_id = install_old_media(&mut planning_session, PlaybackState::Paused);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(113).expect("test request id is non-zero"),
    );
    let mut unsupported_track = fake_track(1, TrackKind::Video);
    unsupported_track.codec_id = "V_UNSUPPORTED".to_owned();
    let (resource_port, resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    planning_session.stage_prepared_media_install(
        request_id,
        prepared_media_from_tracks(vec![unsupported_track]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::VideoStreamConfiguration
    ));
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        0
    );
    assert_eq!(
        planning_session.snapshot().media_instance_id,
        Some(old_instance_id)
    );
    assert_eq!(planning_session.playback_state(), PlaybackState::Paused);
}

/// Неполный AV1 не должен снова превратиться в unprobed detached request:
/// strong transaction завершается до resource boundary и сохраняет old media.
#[test]
fn incomplete_av1_never_requests_unprobed_backend_resource() {
    let mut session = PlayerSession::new();
    let old_instance_id = install_old_media(&mut session, PlaybackState::Paused);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(112).expect("test request id is non-zero"),
    );
    let mut av1_track = fake_track(1, TrackKind::Video);
    av1_track.codec_id = "V_AV1".to_owned();
    let mut av1_metadata = VideoTrackMetadata::empty();
    av1_metadata.coded_width = Some(1_920);
    av1_metadata.coded_height = Some(1_080);
    av1_track.video = Some(av1_metadata);
    let (resource_port, resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        prepared_media_from_tracks(vec![av1_track]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );

    assert!(receipt.try_take_ready_to_commit().is_none());
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed { failure, .. })
            if failure.stage == MediaInstallFailureStage::VideoStreamConfiguration
                && failure.error.message.contains("codec-private")
    ));
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        0
    );
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Paused);
}

#[test]
fn media_without_audio_or_video_never_requests_backend_and_commits_paused() {
    let mut session = PlayerSession::new();
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(105).expect("test request id is non-zero"),
    );
    let (resource_port, resource_state) =
        FakeResourcePort::unavailable(DetachedVideoBackendResourceError::Unavailable {
            reason: "must not be requested".to_owned(),
        });
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(receipt.try_take_ready_to_commit().is_some());
    assert_eq!(
        resource_state
            .lock()
            .expect("fake resource state lock")
            .request_count,
        0
    );

    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let Some(MediaInstallCompletion::Installed {
        media_instance_id, ..
    }) = receipt.try_take_completion()
    else {
        panic!("audio/video-absent candidate must install");
    };
    assert_ne!(media_instance_id, old_instance_id);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(session.pipeline.selected_audio_track_id().is_none());
    assert!(session.pipeline.selected_video_track_id().is_none());
}

#[test]
fn supersede_and_shutdown_cancel_only_the_exact_pre_barrier_candidate() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);
    let first_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(106).expect("test request id is non-zero"),
    );
    let second_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(107).expect("test request id is non-zero"),
    );
    let (first_port, first_resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (first_receipt, first_install_port) = MediaInstallReceipt::new(first_request_id);
    session.stage_prepared_media_install(
        first_request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        first_install_port,
        Box::new(first_port),
    );
    assert!(first_receipt.try_take_ready_to_commit().is_some());

    let (second_port, second_resource_state) =
        FakeResourcePort::available(SharedFakeVideoDecoderThread::new());
    let (second_receipt, second_install_port) = MediaInstallReceipt::new(second_request_id);
    session.stage_prepared_media_install(
        second_request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        second_install_port,
        Box::new(second_port),
    );

    assert!(matches!(
        first_receipt.try_take_completion(),
        Some(MediaInstallCompletion::Cancelled {
            cause: MediaInstallCancellationCause::Superseded,
            ..
        })
    ));
    assert!(matches!(
        first_resource_state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .last(),
        Some(DetachedVideoBackendCandidateStatus::Cancelled {
            cause: DetachedVideoBackendCandidateCancellationCause::Superseded,
            ..
        })
    ));
    assert!(second_receipt.try_take_ready_to_commit().is_some());
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit {
                request_id: first_request_id,
            },
        )),
        MediaInstallControlOutcome::StaleRequest
    );

    assert_eq!(
        session
            .cancel_active_staged_media_install(MediaInstallCancellationCause::LifecycleShutdown,),
        Some(MediaInstallControlOutcome::CancellationAccepted)
    );
    assert!(matches!(
        second_receipt.try_take_completion(),
        Some(MediaInstallCompletion::Cancelled {
            cause: MediaInstallCancellationCause::LifecycleShutdown,
            ..
        })
    ));
    assert!(matches!(
        second_resource_state
            .lock()
            .expect("fake resource state lock")
            .statuses
            .last(),
        Some(DetachedVideoBackendCandidateStatus::Cancelled {
            cause: DetachedVideoBackendCandidateCancellationCause::Disconnected,
            ..
        })
    ));
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}

#[test]
fn atomic_switch_defers_leased_old_frame_to_old_decoder_and_never_uses_new_decoder() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let old_decoder = SharedFakeVideoDecoderThread::new();
    session.set_video_backend(StartedVideoBackend::from_decoder_thread(
        test_decode_backend_id().as_str(),
        old_decoder.clone(),
    ));
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session.snapshot.media_instance_id = Some(MediaInstanceId::from_non_zero(
        NonZeroU64::new(91).expect("test instance id is non-zero"),
    ));
    session.set_playback_state(PlaybackState::Playing);
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(1), 777);
    let old_render_generation = session.pipeline.render_generation();
    let old_resource_handle = video_core::FrameResourceHandle(777);
    assert!(session.register_render_lease(old_render_generation, old_resource_handle));

    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(108).expect("test request id is non-zero"),
    );
    let new_decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, _) = FakeResourcePort::available(new_decoder.clone());
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(receipt.try_take_ready_to_commit().is_some());

    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert!(old_decoder.released_handles().is_empty());
    assert!(new_decoder.released_handles().is_empty());

    session.release_render_lease(old_render_generation, old_resource_handle);
    assert_eq!(old_decoder.released_handles(), vec![old_resource_handle]);
    assert!(new_decoder.released_handles().is_empty());
}

#[test]
fn post_commit_runtime_error_stays_on_new_instance_without_hidden_rollback() {
    let mut session = PlayerSession::new();
    let old_instance_id = install_old_media(&mut session, PlaybackState::Paused);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(109).expect("test request id is non-zero"),
    );
    let (resource_port, _) =
        FakeResourcePort::unavailable(DetachedVideoBackendResourceError::Unavailable {
            reason: "video path is absent and must not request this fake".to_owned(),
        });
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Audio]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        Box::new(resource_port),
    );
    assert!(receipt.try_take_ready_to_commit().is_some());
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let Some(MediaInstallCompletion::Installed {
        media_instance_id: new_instance_id,
        ..
    }) = receipt.try_take_completion()
    else {
        panic!("accepted authorization must publish Installed");
    };
    assert_ne!(new_instance_id, old_instance_id);

    session.mark_fatal_error(PlayerError::new(
        PlayerErrorKind::RuntimeError,
        "synthetic post-commit decoder failure",
    ));
    assert_eq!(session.snapshot().media_instance_id, Some(new_instance_id));
    assert_ne!(session.snapshot().media_instance_id, Some(old_instance_id));
}
