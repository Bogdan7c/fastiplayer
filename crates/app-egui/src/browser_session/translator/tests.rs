use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use browser_bridge_protocol::session::page::{RemovalEnd, TabGoneReason};
use browser_bridge_protocol::{
    AppendSequence, DataFrameHeader, MediaTimeOffset, MimeType, PlaybackRate, SeekId, SessionKey,
    decode_data_frame, decode_message,
};
use browser_bridge_session::SessionInstance;
use media_core::{Demuxer, TrackKind};
use pushed_media_source::{
    DownloadCutOutcome, DownloadCutReport, EndOfStreamReport, OpenProgress, PageRemovalOutcome,
    PageRemovalReport, PlayableEnd, PushedSourcePolicy, PushedStreamOpener, PushedStreamStartError,
    pushed_stream,
};

use super::stream_port::{PushedStreamFactory, StartedStream};
use super::*;

// ---- подделка источника: записывает вызовы писателя ------------------------

/// Вызов писателя, как его увидел источник.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WriterCall {
    AddLane(u16, PushedContainer),
    Append(u16, Vec<u8>),
    Abort(u16),
    ChangeContainer(u16, PushedContainer),
    SetTimestampOffset(u16, i64),
    SetDuration(PushedDuration),
    EndOfStream,
    EndDownloadCutShort,
    NotePageSeek(Duration),
    NotePageRemoved(u16, PageRemovedRange),
}

/// Общий журнал вызовов: (номер потока с 1, вызов).
#[derive(Clone, Default)]
struct CallLog(Arc<Mutex<Vec<(usize, WriterCall)>>>);

impl CallLog {
    fn calls(&self) -> Vec<(usize, WriterCall)> {
        self.0.lock().expect("log").clone()
    }
}

/// Ошибка, которую поток «запишет» сам (как демуксер эпизода на своём потоке).
#[derive(Clone, Default)]
struct AsyncFailure(Arc<Mutex<Option<PushedStreamError>>>);

/// Писатель-подделка: помнит объявленные полосы, неизвестной полосе отвечает
/// `UnknownLane`, как настоящий.
struct FakeWriter {
    stream: usize,
    log: CallLog,
    lanes: Vec<LaneId>,
    failure: AsyncFailure,
}

impl FakeWriter {
    fn record(&self, call: WriterCall) {
        self.log.0.lock().expect("log").push((self.stream, call));
    }

    fn declared(&self, lane: LaneId) -> Result<(), PushedStreamError> {
        if self.lanes.contains(&lane) {
            Ok(())
        } else {
            Err(PushedStreamError::UnknownLane { lane })
        }
    }
}

impl StreamWriterPort for FakeWriter {
    fn add_lane(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        self.lanes.push(lane);
        self.record(WriterCall::AddLane(lane.index(), container));
        Ok(())
    }

    fn append(&mut self, lane: LaneId, chunk: &[u8]) -> Result<AppendOutcome, PushedStreamError> {
        self.declared(lane)?;
        self.record(WriterCall::Append(lane.index(), chunk.to_vec()));
        Ok(AppendOutcome::Accepted)
    }

    fn abort(&mut self, lane: LaneId) -> Result<(), PushedStreamError> {
        self.declared(lane)?;
        self.record(WriterCall::Abort(lane.index()));
        Ok(())
    }

    fn change_container(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        self.declared(lane)?;
        self.record(WriterCall::ChangeContainer(lane.index(), container));
        Ok(())
    }

    fn set_timestamp_offset(
        &mut self,
        lane: LaneId,
        offset: TimestampOffset,
    ) -> Result<(), PushedStreamError> {
        self.declared(lane)?;
        self.record(WriterCall::SetTimestampOffset(
            lane.index(),
            offset.as_micros(),
        ));
        Ok(())
    }

    fn note_page_removed(
        &mut self,
        lane: LaneId,
        range: PageRemovedRange,
    ) -> Result<PageRemovalOutcome, PushedStreamError> {
        self.declared(lane)?;
        self.record(WriterCall::NotePageRemoved(lane.index(), range));
        Ok(PageRemovalOutcome::Applied(PageRemovalReport::default()))
    }

    fn set_duration(&mut self, duration: PushedDuration) {
        self.record(WriterCall::SetDuration(duration));
    }

    fn end_of_stream(&mut self) -> Result<EndOfStreamReport, PushedStreamError> {
        self.record(WriterCall::EndOfStream);
        Ok(EndOfStreamReport {
            dropped_incomplete_bytes: 0,
        })
    }

    fn end_download_cut_short(&mut self) -> Result<DownloadCutOutcome, PushedStreamError> {
        self.record(WriterCall::EndDownloadCutShort);
        Ok(DownloadCutOutcome::CutShort(DownloadCutReport {
            playable_end: PlayableEnd::Unknown,
            dropped_incomplete_bytes: 0,
        }))
    }

    fn note_page_seek(&mut self, target: Duration) {
        self.record(WriterCall::NotePageSeek(target));
    }

    fn failure(&self) -> Option<PushedStreamError> {
        self.failure.0.lock().expect("failure").clone()
    }
}

/// Фабрика-подделка: писатель — запись вызовов, сторона открытия и монитор —
/// настоящие (от настоящего потока, который сразу бросается).
#[derive(Default)]
struct FakeFactory {
    started: usize,
    log: CallLog,
    failure: AsyncFailure,
}

impl StreamFactory for FakeFactory {
    fn start_stream(&mut self) -> Result<StartedStream, PushedStreamStartError> {
        self.started += 1;
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (real_writer, opener) = pushed_stream(policy)?;
        let monitor = real_writer.monitor();
        Ok(StartedStream {
            writer: Box::new(FakeWriter {
                stream: self.started,
                log: self.log.clone(),
                lanes: Vec::new(),
                failure: self.failure.clone(),
            }),
            opener,
            monitor,
        })
    }
}

fn fake_translator() -> (ProtocolTranslator<FakeFactory>, CallLog, AsyncFailure) {
    let factory = FakeFactory::default();
    let log = factory.log.clone();
    let failure = factory.failure.clone();
    (
        ProtocolTranslator::new(factory, file_port::unused_file_factory()),
        log,
        failure,
    )
}

// ---- входы ------------------------------------------------------------------

fn session(instance: u64) -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab").expect("valid key"),
        instance: SessionInstance(instance),
    }
}

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("valid generation")
}

fn buffer(index: u16) -> SourceBufferIndex {
    SourceBufferIndex::new(index).expect("valid index")
}

fn message(instance: u64, message: PageSessionMessage) -> SessionInput {
    SessionInput::Message(session(instance), message)
}

fn started(instance: u64, number: u32) -> SessionInput {
    message(
        instance,
        PageSessionMessage::GenerationStarted {
            generation: generation(number),
        },
    )
}

fn added(instance: u64, number: u32, index: u16, mime: &str) -> SessionInput {
    message(
        instance,
        PageSessionMessage::SourceBufferAdded {
            generation: generation(number),
            source_buffer: buffer(index),
            mime_type: MimeType::new(mime).expect("valid MIME"),
        },
    )
}

fn bytes(instance: u64, number: u32, index: u16, payload: &[u8]) -> SessionInput {
    SessionInput::Bytes(
        session(instance),
        OwnedChunk {
            generation: generation(number),
            source_buffer: buffer(index),
            sequence: AppendSequence(0),
            payload: payload.to_vec(),
        },
    )
}

/// Прогоняет входы и возвращает все уведомления.
fn run<F: StreamFactory>(
    translator: &mut ProtocolTranslator<F>,
    inputs: impl IntoIterator<Item = SessionInput>,
) -> Vec<IntakeNotice> {
    let mut out = Vec::new();
    for input in inputs {
        translator.apply(input, &mut out);
    }
    out
}

/// Короткая форма уведомления для сравнения (сторона открытия не сравнима).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    NewGeneration(u64, u32),
    NewFileGeneration(u64, u32),
    BytesAppended(u64),
    Intent(u64, PageIntent),
    SeekAcknowledged(u64, SeekId, u64),
    Reject(u64, u32, StreamRejection),
    Close(u64, SessionCloseReason),
    Sealed(u64),
}

fn seen(notices: &[IntakeNotice]) -> Vec<Seen> {
    notices
        .iter()
        .map(|notice| match notice {
            IntakeNotice::NewGeneration(handoff) if handoff.source.is_file() => {
                Seen::NewFileGeneration(handoff.session.instance.0, handoff.generation.get())
            }
            IntakeNotice::NewGeneration(handoff) => {
                Seen::NewGeneration(handoff.session.instance.0, handoff.generation.get())
            }
            IntakeNotice::BytesAppended { session } => Seen::BytesAppended(session.instance.0),
            IntakeNotice::PageIntent { session, intent } => {
                Seen::Intent(session.instance.0, *intent)
            }
            IntakeNotice::PlayerSeekAcknowledged {
                session,
                seek_id,
                target,
            } => Seen::SeekAcknowledged(session.instance.0, *seek_id, target.as_micros()),
            IntakeNotice::Reject {
                session,
                generation,
                reason,
            } => Seen::Reject(session.instance.0, generation.get(), *reason),
            IntakeNotice::CloseSession { session, reason } => {
                Seen::Close(session.instance.0, *reason)
            }
            IntakeNotice::InstanceSealed { session } => Seen::Sealed(session.instance.0),
        })
        .collect()
}

// ---- подделка: каждое сообщение → свой вызов --------------------------------

#[test]
fn each_mse_message_becomes_its_writer_call_in_wire_order() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4; codecs=\"avc1.42c00a\""),
            added(1, 1, 1, "audio/webm; codecs=\"opus\""),
            bytes(1, 1, 0, b"init"),
            bytes(1, 1, 0, b"media"),
            message(
                1,
                PageSessionMessage::TimestampOffsetChanged {
                    generation: generation(1),
                    source_buffer: buffer(0),
                    offset_us: MediaTimeOffset::from_micros(-5_000).expect("offset"),
                },
            ),
            message(
                1,
                PageSessionMessage::SourceBufferAborted {
                    generation: generation(1),
                    source_buffer: buffer(1),
                },
            ),
            message(
                1,
                PageSessionMessage::SourceBufferTypeChanged {
                    generation: generation(1),
                    source_buffer: buffer(1),
                    mime_type: MimeType::new("video/mp2t").expect("MIME"),
                },
            ),
            message(
                1,
                PageSessionMessage::DurationChanged {
                    generation: generation(1),
                    duration: MediaDuration::Finite {
                        duration_us: MediaTime::from_micros(400_000).expect("time"),
                    },
                },
            ),
            message(
                1,
                PageSessionMessage::EndOfStream {
                    generation: generation(1),
                },
            ),
        ],
    );

    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::AddLane(1, PushedContainer::WebM)),
            (1, WriterCall::Append(0, b"init".to_vec())),
            (1, WriterCall::Append(0, b"media".to_vec())),
            (1, WriterCall::SetTimestampOffset(0, -5_000)),
            (1, WriterCall::Abort(1)),
            (1, WriterCall::ChangeContainer(1, PushedContainer::MpegTs)),
            (
                1,
                WriterCall::SetDuration(PushedDuration::Finite(Duration::from_millis(400)))
            ),
            (1, WriterCall::EndOfStream),
        ]
    );
    // Подсказка «легли байты» — одна на серию кусков подряд.
    assert_eq!(
        seen(&notices),
        vec![Seen::NewGeneration(1, 1), Seen::BytesAppended(1)]
    );
}

#[test]
fn seeks_and_page_playback_become_notices_and_removals_reach_the_source() {
    let (mut translator, log, _failure) = fake_translator();
    let target = MediaTime::from_micros(42_000_000).expect("time");
    let rate = PlaybackRate::from_permille(1500).expect("rate");
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            message(
                1,
                PageSessionMessage::SeekStarted {
                    target_us: target,
                    cause: SeekCause::PageSeek {},
                },
            ),
            message(
                1,
                PageSessionMessage::SeekStarted {
                    target_us: target,
                    cause: SeekCause::PlayerSeek { seek_id: SeekId(7) },
                },
            ),
            message(
                1,
                PageSessionMessage::PagePlayback {
                    request: PagePlaybackRequest::Pause {},
                },
            ),
            message(
                1,
                PageSessionMessage::PagePlayback {
                    request: PagePlaybackRequest::SetRate { rate },
                },
            ),
            message(
                1,
                PageSessionMessage::SourceBufferRemoved {
                    generation: generation(1),
                    source_buffer: buffer(0),
                    start_us: MediaTime::ZERO,
                    end: RemovalEnd::EndOfTimeline {},
                },
            ),
            message(
                1,
                PageSessionMessage::TabGone {
                    reason: TabGoneReason::Closed,
                },
            ),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Intent(1, PageIntent::FollowPageSeek(target)),
            Seen::SeekAcknowledged(1, SeekId(7), 42_000_000),
            Seen::Intent(1, PageIntent::Pause),
            Seen::Intent(1, PageIntent::SetRate(rate)),
        ]
    );
    // Источник узнал о перемотке страницей и о её `remove()` (решение 31.10.2026
    // «помним, пока помнит страница» отменило D6); `tab_gone` он не видит (03c),
    // перемотку по нашей команде — тоже (она его же).
    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::NotePageSeek(Duration::from_secs(42))),
            (
                1,
                WriterCall::NotePageRemoved(
                    0,
                    PageRemovedRange::new(Duration::ZERO, PageRemovedRangeEnd::EndOfTimeline)
                )
            ),
        ]
    );
    assert_eq!(translator.counters().ignored_messages, 0);
    assert_eq!(translator.counters().tab_gone_messages, 1);
}

#[test]
fn page_seek_reaches_the_source_between_the_bytes_around_it_and_before_the_player_intent() {
    let (mut translator, log, _failure) = fake_translator();
    let target = MediaTime::from_micros(90_000_000).expect("time");
    let page_seek = message(
        1,
        PageSessionMessage::SeekStarted {
            target_us: target,
            cause: SeekCause::PageSeek {},
        },
    );
    // Перемотка до первого поколения: источника нет, но плеер всё равно идёт следом.
    let mut notices = run(&mut translator, [page_seek.clone()]);
    assert!(log.calls().is_empty());
    notices.extend(run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4; codecs=\"avc1.42c00a\""),
            bytes(1, 1, 0, b"old point"),
            page_seek,
            bytes(1, 1, 0, b"new point"),
        ],
    ));

    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::Append(0, b"old point".to_vec())),
            (1, WriterCall::NotePageSeek(Duration::from_secs(90))),
            (1, WriterCall::Append(0, b"new point".to_vec())),
        ]
    );
    let intents: Vec<Seen> = seen(&notices)
        .into_iter()
        .filter(|notice| matches!(notice, Seen::Intent(..)))
        .collect();
    assert_eq!(
        intents,
        vec![
            Seen::Intent(1, PageIntent::FollowPageSeek(target)),
            Seen::Intent(1, PageIntent::FollowPageSeek(target)),
        ]
    );
}

// ---- нарушения и отказы ------------------------------------------------------

#[test]
fn bytes_of_an_unknown_source_buffer_close_the_session_once_and_mute_it() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            bytes(1, 1, 3, b"who am I"),
            bytes(1, 1, 0, b"after the violation"),
            bytes(1, 1, 3, b"again"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Close(1, SessionCloseReason::ProtocolViolation),
        ]
    );
    // После просьбы закрыть вход экземпляра в источник не идёт.
    assert_eq!(
        log.calls(),
        vec![(1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4))]
    );
    assert_eq!(translator.counters().closing_instance_inputs, 2);
}

#[test]
fn input_before_any_generation_is_a_protocol_violation() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(&mut translator, [bytes(1, 1, 0, b"early")]);
    assert_eq!(
        seen(&notices),
        vec![Seen::Close(1, SessionCloseReason::ProtocolViolation)]
    );
    assert!(log.calls().is_empty());
}

#[test]
fn unsupported_mime_rejects_the_generation_once_and_the_next_generation_starts_clean() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/x-flv"),
            bytes(1, 1, 0, b"flv bytes"),
            added(1, 1, 1, "video/x-flv"),
            started(1, 2),
            added(1, 2, 0, "video/webm"),
            bytes(1, 2, 0, b"webm"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Reject(1, 1, StreamRejection::UnsupportedCodec),
            Seen::NewGeneration(1, 2),
            Seen::BytesAppended(1),
        ]
    );
    assert_eq!(
        log.calls(),
        vec![
            (2, WriterCall::AddLane(0, PushedContainer::WebM)),
            (2, WriterCall::Append(0, b"webm".to_vec())),
        ]
    );
    assert_eq!(translator.counters().rejected_generation_inputs, 2);
}

#[test]
fn a_failure_recorded_by_the_stream_rejects_the_generation_exactly_once() {
    let (mut translator, log, failure) = fake_translator();
    let mut notices = run(
        &mut translator,
        [started(1, 1), added(1, 1, 0, "video/mp4")],
    );
    // Демуксер эпизода не разобрал байты на своём потоке.
    *failure.0.lock().expect("failure") = Some(PushedStreamError::Demux {
        lane: LaneId::new(0),
        reason: "garbage".to_owned(),
    });
    notices.extend(run(
        &mut translator,
        [bytes(1, 1, 0, b"one"), bytes(1, 1, 0, b"two")],
    ));

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Reject(1, 1, StreamRejection::UnparsableMedia),
        ]
    );
    // Первый кусок ещё дошёл (ошибка обнаружена после вызова), второй — нет.
    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::Append(0, b"one".to_vec())),
        ]
    );
    assert_eq!(translator.counters().rejected_generation_inputs, 1);
}

#[test]
fn old_generation_input_does_not_touch_the_new_generation() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            started(1, 2),
            added(1, 2, 0, "video/webm"),
            bytes(1, 1, 0, b"stale"),
            message(
                1,
                PageSessionMessage::EndOfStream {
                    generation: generation(1),
                },
            ),
            bytes(1, 2, 0, b"fresh"),
        ],
    );

    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            // Заменённое поколение кончается обрывом (04d), а не честным концом.
            (1, WriterCall::EndDownloadCutShort),
            (2, WriterCall::AddLane(0, PushedContainer::WebM)),
            (2, WriterCall::Append(0, b"fresh".to_vec())),
        ]
    );
    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::NewGeneration(1, 2),
            Seen::BytesAppended(1),
        ]
    );
    assert_eq!(translator.counters().stale_inputs, 2);
}

#[test]
fn a_new_instance_starts_from_scratch_and_the_old_one_is_stale() {
    let (mut translator, log, _failure) = fake_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            SessionInput::Ended(session(1)),
            // Новый экземпляр без своего `generation_started`: у него состояния нет.
            bytes(2, 1, 0, b"no generation yet"),
            // Вход старого экземпляра после нового — устаревший.
            bytes(1, 1, 0, b"late"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Sealed(1),
            Seen::Close(2, SessionCloseReason::ProtocolViolation),
        ]
    );
    // Конец экземпляра запечатал его поток как оборванное скачивание: полка
    // доигрывается до конца самой короткой дорожки (решение 133), а не как честный
    // `endOfStream` страницы.
    assert_eq!(
        log.calls(),
        vec![
            (1, WriterCall::AddLane(0, PushedContainer::FragmentedMp4)),
            (1, WriterCall::EndDownloadCutShort),
        ]
    );
    assert_eq!(translator.counters().stale_inputs, 1);
}

/// Конец экземпляра выдаёт уведомление «вход запечатан» строго ПОСЛЕ уведомлений его
/// данных (порядок очереди): по нему главный цикл знает, что всё разобрано. Для
/// экземпляра, от которого ничего не приходило, оно тоже выдаётся.
#[test]
fn the_end_of_an_instance_is_announced_after_its_data_notices() {
    let (mut translator, _log, _failure) = fake_translator();

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            bytes(1, 1, 0, b"init"),
            SessionInput::Ended(session(1)),
            SessionInput::Ended(session(5)),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::BytesAppended(1),
            Seen::Sealed(1),
            Seen::Sealed(5),
        ]
    );
}

mod file_source;
mod log_privacy;
mod page_removal;
mod real_source;
