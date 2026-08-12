//! Focused lifecycle/provenance tests нового параллельного adapter-а.

use std::collections::VecDeque;
use std::io::Read;
use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use bytes::Bytes;
use demux_api::{
    DemuxSniffBudget, OrderedSegmentDiscontinuity, OrderedSegmentReadError, OrderedSegmentSequence,
    PresentationWindowOrderedSegment, PresentationWindowOrderedSegmentReadOutcome,
    PresentationWindowOrderedSegmentSource,
};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, ExactPresentationWindow, PacketPresentationWindow, TimeBase, TrackId, TrackInfo,
    TrackKind, TrackTimestamp,
};
use source_core::CancellationToken;
use symphonia_format_isomp4::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentBaseDecodeTime, FragmentInitializationCodec,
    FragmentInitializationLimits, FragmentInitializationRequest, FragmentInspectionLimits,
    FragmentMediaKind, FragmentReconstructionRequest, FragmentSampleDefaults, FragmentTimescale,
    FragmentTrackId, FragmentTrackReconstructionIntent, FragmentWriteLimits,
    build_fragmented_initialization_segment, reconstruct_media_fragment,
};

use super::{
    ActiveFragment, InitializationMediaReader, PresentationWindowOrderedIsoMp4Demuxer,
    PresentationWindowOrderedIsoMp4Error, PresentationWindowOrderedTrackField, build_registry,
    validate_stable_track,
};
use crate::DemuxerOptions;

/// Канонический audio fragment переиспользуется без копии fixture corpus.
const AUDIO_FRAGMENT: &[u8] =
    include_bytes!("../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-0.bin");

/// Двухчастный reader сохраняет exact bytes при коротких reads и пересечении границы.
#[test]
fn initialization_media_reader_crosses_boundary_without_skips() {
    let mut reader =
        InitializationMediaReader::new(Bytes::from_static(b"abc"), Bytes::from_static(b"DEFG"));
    let mut buffer = [0_u8; 2];

    assert_eq!(reader.read(&mut buffer).expect("first short read"), 2);
    assert_eq!(&buffer, b"ab");
    assert_eq!(reader.read(&mut buffer).expect("boundary read"), 2);
    assert_eq!(&buffer, b"cD");
    assert_eq!(reader.read(&mut buffer).expect("media short read"), 2);
    assert_eq!(&buffer, b"EF");
    assert_eq!(reader.read(&mut buffer).expect("media tail"), 1);
    assert_eq!(buffer[0], b'G');
    assert_eq!(reader.read(&mut buffer).expect("terminal read"), 0);
    assert_eq!(reader.read(&mut []).expect("empty terminal read"), 0);
}

/// In-memory source с explicit readiness/terminal outcomes.
struct MemoryWindowSource {
    outcomes: VecDeque<PresentationWindowOrderedSegmentReadOutcome>,
    failure: Option<OrderedSegmentReadError>,
}

impl MemoryWindowSource {
    /// Создаёт успешный ordered source.
    fn new(outcomes: Vec<PresentationWindowOrderedSegmentReadOutcome>) -> Self {
        Self {
            outcomes: outcomes.into(),
            failure: None,
        }
    }

    /// Создаёт source, который возвращает typed failure.
    fn failed(failure: OrderedSegmentReadError) -> Self {
        Self {
            outcomes: VecDeque::new(),
            failure: Some(failure),
        }
    }
}

impl PresentationWindowOrderedSegmentSource for MemoryWindowSource {
    /// Возвращает следующий заранее заданный outcome.
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<PresentationWindowOrderedSegmentReadOutcome, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        Ok(self
            .outcomes
            .pop_front()
            .unwrap_or(PresentationWindowOrderedSegmentReadOutcome::EndOfStream))
    }
}

/// Source отменяет shared token после выдачи первого media и до inner open.
struct CancelAfterFirstMediaSource {
    outcomes: VecDeque<PresentationWindowOrderedSegmentReadOutcome>,
    cancellation: CancellationToken,
    pulls: usize,
}

impl PresentationWindowOrderedSegmentSource for CancelAfterFirstMediaSource {
    /// Возвращает init/media и активирует cancellation сразу после второго pull-а.
    fn next_segment(
        &mut self,
        _cancellation: &CancellationToken,
    ) -> Result<PresentationWindowOrderedSegmentReadOutcome, OrderedSegmentReadError> {
        self.pulls += 1;
        let outcome = self
            .outcomes
            .pop_front()
            .unwrap_or(PresentationWindowOrderedSegmentReadOutcome::EndOfStream);
        if self.pulls == 2 {
            self.cancellation.cancel();
        }
        Ok(outcome)
    }
}

/// Fake inner нужен только для lifecycle events, которые canonical fixture не генерирует.
struct EventDemuxer {
    tracks: Vec<TrackInfo>,
    events: VecDeque<DemuxReadEvent>,
    cancel_on_read: Option<CancellationToken>,
}

impl EventDemuxer {
    /// Создаёт deterministic inner с заданными событиями.
    fn new(tracks: Vec<TrackInfo>, events: Vec<DemuxReadEvent>) -> Self {
        Self {
            tracks,
            events: events.into(),
            cancel_on_read: None,
        }
    }

    /// Добавляет отмену между inner read и внешней publication fence.
    fn cancelling(
        tracks: Vec<TrackInfo>,
        event: DemuxReadEvent,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            tracks,
            events: VecDeque::from([event]),
            cancel_on_read: Some(cancellation),
        }
    }
}

impl Demuxer for EventDemuxer {
    /// Возвращает immutable fake track snapshot.
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Fake fragment не объявляет общую длительность.
    fn duration(&self) -> Option<Duration> {
        None
    }

    /// Возвращает следующее событие и при необходимости активирует cancellation fence.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if let Some(cancellation) = self.cancel_on_read.take() {
            cancellation.cancel();
        }
        Ok(self
            .events
            .pop_front()
            .unwrap_or(DemuxReadEvent::EndOfStream))
    }

    /// Seek для fake inner не участвует в тестовом сценарии.
    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("fake inner не поддерживает seek"))
    }
}

/// Explicit test sniff budget.
fn sniff_budget() -> DemuxSniffBudget {
    DemuxSniffBudget::new(
        NonZeroUsize::new(4_096).expect("sniff bytes"),
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    )
    .expect("sniff budget")
}

/// Детерминированно строит generic AAC init через единственного ISO owner-а.
fn audio_initialization() -> Bytes {
    let sample_rate =
        FragmentAacSampleRate::try_new(48_000).expect("48 kHz помещается в init contract");
    let channels = FragmentAacChannelCount::try_new(2).expect("stereo помещается");
    let asc = FragmentAacAudioSpecificConfig::try_new(&[0x11, 0x90]).expect("AAC-LC ASC");
    let codec =
        FragmentAacLcConfiguration::try_new(sample_rate, channels, asc).expect("AAC config");
    let limits = FragmentInitializationLimits::builder()
        .maximum_output_bytes(16 * 1024)
        .maximum_codec_configuration_bytes(1_024)
        .build()
        .expect("init limits");
    let cancellation = || false;
    let request = FragmentInitializationRequest::new(
        FragmentTrackId::new(NonZeroU32::MIN),
        FragmentTimescale::new(NonZeroU32::new(10_000_000).expect("timescale")),
        FragmentInitializationCodec::AacLowComplexity(codec),
        &limits,
        &cancellation,
    );
    Bytes::from(
        build_fragmented_initialization_segment(request)
            .expect("AAC init")
            .into_bytes(),
    )
}

/// Канонизирует PIFF capture через публичного ISO owner-а, не копируя fixture corpus.
fn canonical_audio_fragment() -> Bytes {
    let inspection_limits = FragmentInspectionLimits::builder()
        .max_input_bytes(256 * 1024)
        .max_box_count(64)
        .max_box_depth(4)
        .max_traf_count(1)
        .max_trun_count(8)
        .max_samples(512)
        .max_sample_table_bytes(64 * 1024)
        .max_box_payload_bytes(256 * 1024)
        .build()
        .expect("полные inspection budgets");
    let write_limits = FragmentWriteLimits::try_new(512 * 1024).expect("ненулевой output budget");
    let track_intent = FragmentTrackReconstructionIntent::new(
        FragmentTrackId::new(NonZeroU32::MIN),
        FragmentBaseDecodeTime::new(0),
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        FragmentSampleDefaults::absent(),
    );
    let cancellation = || false;
    let request = FragmentReconstructionRequest::new(
        AUDIO_FRAGMENT,
        symphonia_format_isomp4::FragmentCompositionOffsetSemantics::IsoBmffVersioned,
        track_intent,
        &inspection_limits,
        write_limits,
        &cancellation,
    );
    Bytes::from(
        reconstruct_media_fragment(request)
            .expect("canonical audio fragment")
            .into_bytes(),
    )
}

/// Создаёт exact bounded window в clock канонического AAC track-а.
fn bounded_window(start: i64, end_exclusive: i64) -> PacketPresentationWindow {
    let track_id = TrackId::new(1);
    let time_base = TimeBase::new(1, 10_000_000).expect("10 MHz time base");
    PacketPresentationWindow::Bounded(
        ExactPresentationWindow::new(
            TrackTimestamp::new(track_id, start, time_base),
            TrackTimestamp::new(track_id, end_exclusive, time_base),
        )
        .expect("bounded window"),
    )
}

/// Строит init segment.
fn initialization(sequence: u64) -> PresentationWindowOrderedSegment {
    PresentationWindowOrderedSegment::Initialization {
        sequence: OrderedSegmentSequence::new(sequence),
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: audio_initialization(),
    }
}

/// Строит media fragment с exact window intent.
fn media(sequence: u64, window: PacketPresentationWindow) -> PresentationWindowOrderedSegment {
    PresentationWindowOrderedSegment::Media {
        sequence: OrderedSegmentSequence::new(sequence),
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: canonical_audio_fragment(),
        presentation_window: window,
    }
}

/// Открывает production adapter над заданными outcomes.
fn open(
    outcomes: Vec<PresentationWindowOrderedSegmentReadOutcome>,
) -> Result<PresentationWindowOrderedIsoMp4Demuxer, PresentationWindowOrderedIsoMp4Error> {
    PresentationWindowOrderedIsoMp4Demuxer::new(
        Box::new(MemoryWindowSource::new(outcomes)),
        CancellationToken::new(),
        sniff_budget(),
        DemuxerOptions::default(),
    )
}

/// Composition-injected registry сохраняет тот же exact ISO-BMFF open path.
#[test]
fn injected_registry_opens_canonical_fragment_without_parallel_factory() {
    let registry = build_registry(DemuxerOptions::default()).expect("production ISO registry");
    let demuxer = PresentationWindowOrderedIsoMp4Demuxer::new_with_registry(
        Box::new(MemoryWindowSource::new(vec![
            ready(initialization(0)),
            ready(media(1, PacketPresentationWindow::Unbounded)),
        ])),
        CancellationToken::new(),
        sniff_budget(),
        std::sync::Arc::new(registry),
    )
    .expect("injected registry opens canonical fragment");

    assert_eq!(demuxer.tracks().len(), 1);
    assert!(matches!(demuxer.tracks()[0].kind, TrackKind::Audio));
}

/// Извлекает open error без требования Debug к runtime trait objects.
fn expect_open_error(
    result: Result<PresentationWindowOrderedIsoMp4Demuxer, PresentationWindowOrderedIsoMp4Error>,
    context: &str,
) -> PresentationWindowOrderedIsoMp4Error {
    match result {
        Ok(_) => panic!("{context}: ожидалась ошибка"),
        Err(error) => error,
    }
}

/// Сокращает fixture declarations.
fn ready(segment: PresentationWindowOrderedSegment) -> PresentationWindowOrderedSegmentReadOutcome {
    PresentationWindowOrderedSegmentReadOutcome::Segment(segment)
}

/// Lifecycle init/media и source errors остаются typed.
#[test]
fn initialization_media_lifecycle_is_strict() {
    let media_before_init = expect_open_error(
        open(vec![ready(media(0, PacketPresentationWindow::Unbounded))]),
        "media before init",
    );
    assert!(matches!(
        media_before_init,
        PresentationWindowOrderedIsoMp4Error::MediaBeforeInitialization
    ));

    let duplicate_init = expect_open_error(
        open(vec![ready(initialization(0)), ready(initialization(1))]),
        "duplicate init",
    );
    assert!(matches!(
        duplicate_init,
        PresentationWindowOrderedIsoMp4Error::DuplicateInitialization
    ));

    let non_monotonic = expect_open_error(
        open(vec![
            ready(initialization(5)),
            ready(media(5, PacketPresentationWindow::Unbounded)),
        ]),
        "non-monotonic",
    );
    assert!(matches!(
        non_monotonic,
        PresentationWindowOrderedIsoMp4Error::NonMonotonicSequence { .. }
    ));

    let mut discontinuous_media = media(1, PacketPresentationWindow::Unbounded);
    if let PresentationWindowOrderedSegment::Media { discontinuity, .. } = &mut discontinuous_media
    {
        *discontinuity = OrderedSegmentDiscontinuity::StartsNewTimeline;
    }
    let discontinuity = expect_open_error(
        open(vec![ready(initialization(0)), ready(discontinuous_media)]),
        "discontinuity",
    );
    assert!(matches!(
        discontinuity,
        PresentationWindowOrderedIsoMp4Error::DiscontinuityRequiresSessionReset
    ));

    let source_failure = expect_open_error(
        PresentationWindowOrderedIsoMp4Demuxer::new(
            Box::new(MemoryWindowSource::failed(
                OrderedSegmentReadError::Failed {
                    reason: "safe fixture failure".to_owned(),
                },
            )),
            CancellationToken::new(),
            sniff_budget(),
            DemuxerOptions::default(),
        ),
        "source failure",
    );
    assert!(matches!(
        source_failure,
        PresentationWindowOrderedIsoMp4Error::Source(_)
    ));
}

/// Одинаковые PTS двух audio fragments получают окна строго по segment provenance.
#[test]
fn overlapping_audio_pts_keep_distinct_fragment_windows_and_readiness() {
    let first_window = bounded_window(0, 39_680_000);
    let second_window = bounded_window(39_680_000, 79_573_333);
    let retry_hint = DemuxRetryHint::new(Duration::from_millis(5)).expect("retry hint");
    let mut demuxer = open(vec![
        ready(initialization(0)),
        ready(media(1, first_window)),
        PresentationWindowOrderedSegmentReadOutcome::TemporarilyUnavailable(retry_hint),
        ready(media(2, second_window)),
        PresentationWindowOrderedSegmentReadOutcome::EndOfStream,
    ])
    .expect("open two fragments");

    assert_eq!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable {
            reason: media_core::TimelineNotSeekableReason::SourceNotSeekable
        }
    );
    let mut first_packets = 0_usize;
    loop {
        match demuxer.next_event().expect("first fragment event") {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.presentation_window(), first_window);
                first_packets += 1;
            }
            DemuxReadEvent::TemporarilyUnavailable(actual) => {
                assert_eq!(actual, retry_hint);
                break;
            }
            other => panic!("unexpected first-fragment event: {other:?}"),
        }
    }
    assert!(first_packets > 0);

    let mut second_packets = 0_usize;
    loop {
        match demuxer.next_event().expect("second fragment event") {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.presentation_window(), second_window);
                second_packets += 1;
            }
            DemuxReadEvent::EndOfStream => break,
            other => panic!("unexpected second-fragment event: {other:?}"),
        }
    }
    assert!(second_packets > 0);
    assert_eq!(
        demuxer.next_event().expect("terminal remains terminal"),
        DemuxReadEvent::EndOfStream
    );
}

/// Explicit Unbounded никогда не превращается в fake bounded window.
#[test]
fn unbounded_fragment_packets_remain_unbounded() {
    let mut demuxer = open(vec![
        ready(initialization(0)),
        ready(media(1, PacketPresentationWindow::Unbounded)),
        PresentationWindowOrderedSegmentReadOutcome::EndOfStream,
    ])
    .expect("open unbounded");
    let mut packets = 0_usize;
    loop {
        match demuxer.next_event().expect("unbounded event") {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(
                    packet.presentation_window(),
                    PacketPresentationWindow::Unbounded
                );
                packets += 1;
            }
            DemuxReadEvent::EndOfStream => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(packets > 0);
}

/// Invalid media и bounded window clock fail typed.
#[test]
fn inner_open_and_window_assignment_fail_typed() {
    let invalid_media = PresentationWindowOrderedSegment::Media {
        sequence: OrderedSegmentSequence::new(1),
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: Bytes::from_static(b"not an ISO fragment"),
        presentation_window: PacketPresentationWindow::Unbounded,
    };
    let open_error = expect_open_error(
        open(vec![ready(initialization(0)), ready(invalid_media)]),
        "inner open",
    );
    assert!(matches!(
        open_error,
        PresentationWindowOrderedIsoMp4Error::InnerOpen(_)
    ));

    let wrong_track = PacketPresentationWindow::Bounded(
        ExactPresentationWindow::new(
            TrackTimestamp::new(TrackId::new(99), 0, TimeBase::new(1, 10_000_000).unwrap()),
            TrackTimestamp::new(
                TrackId::new(99),
                39_680_000,
                TimeBase::new(1, 10_000_000).unwrap(),
            ),
        )
        .unwrap(),
    );
    let mut demuxer = open(vec![
        ready(initialization(0)),
        ready(media(1, wrong_track)),
        PresentationWindowOrderedSegmentReadOutcome::EndOfStream,
    ])
    .expect("inner itself opens");
    let assignment = demuxer.next_event().expect_err("window assignment");
    assert!(
        assignment
            .downcast_ref::<PresentationWindowOrderedIsoMp4Error>()
            .is_some_and(|error| matches!(
                error,
                PresentationWindowOrderedIsoMp4Error::PresentationWindowAssignment(_)
            ))
    );
}

/// Stable snapshot validation различает decoder-facing drift поля.
#[test]
fn stable_track_validation_rejects_drift() {
    let demuxer = open(vec![
        ready(initialization(0)),
        ready(media(1, PacketPresentationWindow::Unbounded)),
    ])
    .expect("open baseline");
    let expected = demuxer.tracks().to_vec();
    let mut drifted = expected.clone();
    drifted[0].sample_rate = Some(44_100);
    let error = validate_stable_track(&expected, &drifted).expect_err("sample rate drift");
    assert!(matches!(
        error,
        PresentationWindowOrderedIsoMp4Error::IncompatibleTrack {
            field: PresentationWindowOrderedTrackField::SampleRate
        }
    ));
    assert!(validate_stable_track(&expected, &expected).is_ok());
}

/// Initial readiness/cancellation и seek rejection имеют отдельную семантику.
#[test]
fn initial_readiness_cancellation_and_seek_are_explicit() {
    let readiness = expect_open_error(
        open(vec![
            PresentationWindowOrderedSegmentReadOutcome::TemporarilyUnavailable(
                DemuxRetryHint::new(Duration::from_millis(5)).unwrap(),
            ),
        ]),
        "initial readiness",
    );
    assert!(matches!(
        readiness,
        PresentationWindowOrderedIsoMp4Error::InitialSegmentsTemporarilyUnavailable {
            retry_hint: actual
        } if actual == DemuxRetryHint::new(Duration::from_millis(5)).unwrap()
    ));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = expect_open_error(
        PresentationWindowOrderedIsoMp4Demuxer::new(
            Box::new(MemoryWindowSource::new(vec![
                ready(initialization(0)),
                ready(media(1, PacketPresentationWindow::Unbounded)),
            ])),
            cancellation,
            sniff_budget(),
            DemuxerOptions::default(),
        ),
        "cancel before pull/open",
    );
    assert!(matches!(
        cancelled,
        PresentationWindowOrderedIsoMp4Error::Cancelled
    ));

    let cancellation = CancellationToken::new();
    let cancelled_before_open = expect_open_error(
        PresentationWindowOrderedIsoMp4Demuxer::new(
            Box::new(CancelAfterFirstMediaSource {
                outcomes: VecDeque::from([
                    ready(initialization(0)),
                    ready(media(1, PacketPresentationWindow::Unbounded)),
                ]),
                cancellation: cancellation.clone(),
                pulls: 0,
            }),
            cancellation,
            sniff_budget(),
            DemuxerOptions::default(),
        ),
        "cancel before inner open",
    );
    assert!(matches!(
        cancelled_before_open,
        PresentationWindowOrderedIsoMp4Error::Cancelled
    ));

    let mut demuxer = open(vec![
        ready(initialization(0)),
        ready(media(1, PacketPresentationWindow::Unbounded)),
    ])
    .unwrap();
    let seek = demuxer
        .seek(Duration::from_secs(1))
        .expect_err("ordered adapter not seekable");
    assert!(seek.downcast_ref::<media_core::MediaDemuxError>().is_some());
}

/// `TracksChanged` закрывает adapter навсегда, потому что decoder reset ему не принадлежит.
#[test]
fn tracks_changed_fails_closed() {
    let mut demuxer = open(vec![
        ready(initialization(0)),
        ready(media(1, PacketPresentationWindow::Unbounded)),
    ])
    .expect("open baseline");
    let tracks = demuxer.tracks().to_vec();
    demuxer.active_fragment = Some(ActiveFragment {
        demuxer: Box::new(EventDemuxer::new(
            tracks.clone(),
            vec![DemuxReadEvent::TracksChanged(DemuxTrackListUpdate {
                tracks,
                duration: None,
            })],
        )),
        presentation_window: PacketPresentationWindow::Unbounded,
    });

    for attempt in 0..2 {
        let error = demuxer
            .next_event()
            .expect_err("TracksChanged должен закрыть adapter");
        assert!(
            error
                .downcast_ref::<PresentationWindowOrderedIsoMp4Error>()
                .is_some_and(|typed| matches!(
                    typed,
                    PresentationWindowOrderedIsoMp4Error::TracksChanged
                )),
            "attempt {attempt} должен вернуть тот же fail-closed contract"
        );
    }
}

/// Отмена после inner read не позволяет уже прочитанному packet-у пересечь boundary.
#[test]
fn cancellation_fences_packet_publication() {
    let cancellation = CancellationToken::new();
    let mut demuxer = PresentationWindowOrderedIsoMp4Demuxer::new(
        Box::new(MemoryWindowSource::new(vec![
            ready(initialization(0)),
            ready(media(1, PacketPresentationWindow::Unbounded)),
        ])),
        cancellation.clone(),
        sniff_budget(),
        DemuxerOptions::default(),
    )
    .expect("open baseline");
    let tracks = demuxer.tracks().to_vec();
    let packet = loop {
        let event = demuxer
            .active_fragment
            .as_mut()
            .expect("active canonical fragment")
            .demuxer
            .next_event()
            .expect("canonical packet read");
        if let DemuxReadEvent::Packet(packet) = event {
            break packet;
        }
    };
    demuxer.active_fragment = Some(ActiveFragment {
        demuxer: Box::new(EventDemuxer::cancelling(
            tracks,
            DemuxReadEvent::Packet(packet),
            cancellation,
        )),
        presentation_window: PacketPresentationWindow::Unbounded,
    });

    let error = demuxer
        .next_event()
        .expect_err("publication fence должна увидеть cancellation");
    assert!(
        error
            .downcast_ref::<PresentationWindowOrderedIsoMp4Error>()
            .is_some_and(|typed| matches!(typed, PresentationWindowOrderedIsoMp4Error::Cancelled))
    );
}
