//! Переходник с настоящим источником `pushed-media-source`: записанные потоки
//! протокола и зашифрованный init.

use super::*;

fn protocol_fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../browser-bridge-protocol/fixtures")
}

/// Записанный поток протокола → входы очереди сеанса 1 (как их сложил бы сервер).
fn recorded_inputs(name: &str) -> Vec<SessionInput> {
    let directory = protocol_fixtures().join("streams").join(name);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("stream.json")).expect("manifest"))
            .expect("manifest JSON");
    let read = |file: &serde_json::Value| {
        std::fs::read(directory.join(file.as_str().expect("file name"))).expect("step file")
    };
    manifest["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| match (step.get("text"), step.get("binary")) {
            (Some(file), None) => message(1, decode_message(&read(file)).expect("message")),
            (None, Some(file)) => {
                let frame_bytes = read(file);
                let frame = decode_data_frame(&frame_bytes).expect("frame");
                let DataFrameHeader::SourceBufferBytes(header) = frame.header else {
                    panic!("recorded streams carry source buffer bytes");
                };
                SessionInput::Bytes(
                    session(1),
                    OwnedChunk {
                        generation: header.generation,
                        source_buffer: header.source_buffer,
                        sequence: header.sequence,
                        payload: frame.payload.to_vec(),
                    },
                )
            }
            _ => panic!("a step is either text or binary"),
        })
        .collect()
}

fn real_translator() -> ProtocolTranslator<PushedStreamFactory> {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    ProtocolTranslator::new(
        PushedStreamFactory::new(policy),
        super::super::file_port::unused_file_factory(),
    )
}

/// Открывает поток так, как это сделает сторона подготовки (неблокирующие шаги).
fn open_demuxer(opener: &mut PushedStreamOpener) -> Box<dyn Demuxer> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match opener.poll_open().expect("the stream opens") {
            OpenProgress::Ready(opened) => return Box::new(opened.demuxer),
            OpenProgress::Waiting(hint) => {
                assert!(Instant::now() < deadline, "the stream did not open in time");
                std::thread::sleep(hint.retry_after());
            }
        }
    }
}

#[test]
fn recorded_two_buffer_stream_through_the_translator_opens_with_both_tracks() {
    let mut translator = real_translator();
    let notices = run(
        &mut translator,
        recorded_inputs("two-source-buffers-mid-box-cuts"),
    );
    let mut handoffs: Vec<Box<GenerationHandoff>> = notices
        .into_iter()
        .filter_map(|notice| match notice {
            IntakeNotice::NewGeneration(handoff) => Some(handoff),
            IntakeNotice::Reject { reason, .. } => panic!("unexpected rejection {reason:?}"),
            IntakeNotice::CloseSession { reason, .. } => panic!("unexpected close {reason:?}"),
            _ => None,
        })
        .collect();
    assert_eq!(handoffs.len(), 1);
    let handoff = &mut handoffs[0];
    assert_eq!(handoff.generation, generation(1));

    let GenerationSource::Segments { opener, monitor } = &mut handoff.source else {
        panic!("a source buffer generation must hand over segments");
    };
    let demuxer = open_demuxer(opener);
    let mut kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|track| track.kind).collect();
    kinds.sort_by_key(|kind| format!("{kind:?}"));
    assert_eq!(kinds, vec![TrackKind::Audio, TrackKind::Video]);
    assert_eq!(demuxer.duration(), Some(Duration::from_millis(400)));
    // Монитор смотрит на тот же поток: принятые сегменты он видит.
    assert!(monitor.held_segment_bytes() > 0);
}

/// fMP4 фикстуры протокола с `avc1` в описании образца, переименованным в `encv`
/// (так выглядит зашифрованное видео для источника).
fn protected_video_mp4() -> Vec<u8> {
    let mut file = std::fs::read(protocol_fixtures().join("media/video.mp4")).expect("video.mp4");
    let moov = file
        .windows(4)
        .position(|window| window == b"moov")
        .expect("moov box");
    let entry = file[moov..]
        .windows(4)
        .position(|window| window == b"avc1")
        .expect("avc1 sample entry")
        + moov;
    file[entry..entry + 4].copy_from_slice(b"encv");
    file
}

#[test]
fn protected_initialization_is_rejected_once_and_later_bytes_are_dropped() {
    let mut translator = real_translator();
    let protected = protected_video_mp4();
    let (head, tail) = protected.split_at(protected.len() / 2);
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4; codecs=\"avc1.42c00a\""),
            bytes(1, 1, 0, head),
            bytes(1, 1, 0, tail),
            message(
                1,
                PageSessionMessage::EndOfStream {
                    generation: generation(1),
                },
            ),
        ],
    );

    let seen = seen(&notices);
    let rejections: Vec<&Seen> = seen
        .iter()
        .filter(|notice| matches!(notice, Seen::Reject(..)))
        .collect();
    assert_eq!(
        rejections,
        vec![&Seen::Reject(1, 1, StreamRejection::ProtectedContent)]
    );
    assert!(
        !seen.iter().any(|notice| matches!(notice, Seen::Close(..))),
        "DRM is a rejection of the generation, not a protocol violation"
    );
    // init целиком в первой половине: отказ на ней, а вторая половина и конец
    // потока выброшены со счётом.
    assert_eq!(translator.counters().rejected_generation_inputs, 2);
}

#[test]
fn page_removal_through_the_translator_releases_played_bytes_of_the_real_source() {
    let mut translator = real_translator();
    let notices = run(
        &mut translator,
        recorded_inputs("two-source-buffers-mid-box-cuts"),
    );
    let mut handoff = notices
        .into_iter()
        .find_map(|notice| match notice {
            IntakeNotice::NewGeneration(handoff) => Some(handoff),
            _ => None,
        })
        .expect("a generation");
    let GenerationSource::Segments { opener, monitor } = &mut handoff.source else {
        panic!("a source buffer generation must hand over segments");
    };
    let monitor = monitor.clone();
    // Плеер сыграл всё: дочитываем демуксер до конца и сообщаем позицию за концом.
    let mut demuxer = open_demuxer(opener);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "the stream did not end");
        match demuxer.next_event().expect("readable") {
            media_core::DemuxReadEvent::EndOfStream => break,
            media_core::DemuxReadEvent::TemporarilyUnavailable(hint) => {
                std::thread::sleep(hint.retry_after());
            }
            _ => {}
        }
    }
    monitor.note_playback_position(Duration::from_secs(10));
    let held_before = monitor.held_segment_bytes();
    // Страница стёрла у себя всё на обоих SourceBuffer.
    let removals: Vec<SessionInput> = (0..2)
        .map(|index| {
            message(
                1,
                PageSessionMessage::SourceBufferRemoved {
                    generation: generation(1),
                    source_buffer: buffer(index),
                    start_us: MediaTime::ZERO,
                    end: RemovalEnd::EndOfTimeline {},
                },
            )
        })
        .collect();
    let notices = run(&mut translator, removals);
    assert!(seen(&notices).is_empty(), "no rejection or close");
    // Потоки эпизодов отпускают свои копии чуть позже чтения: ждём итог.
    while monitor.held_segment_bytes() >= held_before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let held_after = monitor.held_segment_bytes();
    assert!(
        held_after < held_before,
        "played segments were released: {held_before} -> {held_after}"
    );
    // Остаются только init-сегменты полос.
    assert!(held_after > 0);
}

/// Демуксер поколения из уведомлений переходника (первое поколение).
fn opened_generation_demuxer(notices: Vec<IntakeNotice>) -> Box<dyn Demuxer> {
    let mut handoff = notices
        .into_iter()
        .find_map(|notice| match notice {
            IntakeNotice::NewGeneration(handoff) => Some(handoff),
            _ => None,
        })
        .expect("a generation");
    let GenerationSource::Segments { opener, .. } = &mut handoff.source else {
        panic!("a source buffer generation must hand over segments");
    };
    open_demuxer(opener)
}

/// Страница ушла посреди потока (без своего `endOfStream`): конец экземпляра — это
/// оборванное скачивание, и плеер узнаёт, что конец данных обрезан (позиция на
/// `Ended` не прыгнет в длительность, решение 133).
#[test]
fn a_session_ending_mid_stream_cuts_the_download_short_for_the_player() {
    let mut translator = real_translator();
    let mut inputs: Vec<SessionInput> = recorded_inputs("two-source-buffers-mid-box-cuts")
        .into_iter()
        .filter(|input| {
            !matches!(
                input,
                SessionInput::Message(_, PageSessionMessage::EndOfStream { .. })
            )
        })
        .collect();
    inputs.push(SessionInput::Ended(session(1)));

    let demuxer = opened_generation_demuxer(run(&mut translator, inputs));

    assert!(matches!(
        demuxer.data_end(),
        media_core::DemuxDataEnd::CutShort {
            playable_end: Some(_)
        }
    ));
}

/// Страница честно закончила поток, а потом ушла: скачанное — всё медиа, конец
/// экземпляра его не обрывает, и плеер доигрывает его как файл.
#[test]
fn a_session_ending_after_an_honest_end_of_stream_keeps_the_complete_end() {
    let mut translator = real_translator();
    let mut inputs = recorded_inputs("two-source-buffers-mid-box-cuts");
    assert!(
        inputs.iter().any(|input| matches!(
            input,
            SessionInput::Message(_, PageSessionMessage::EndOfStream { .. })
        )),
        "the recorded stream ends honestly"
    );
    inputs.push(SessionInput::Ended(session(1)));

    let demuxer = opened_generation_demuxer(run(&mut translator, inputs));

    assert_eq!(demuxer.data_end(), media_core::DemuxDataEnd::Complete);
}

/// 04d, путь C: страница открыла новое поколение (`MediaSource`) посреди потока —
/// заменённое поколение кончается ОБРЫВОМ, а не честным концом: плеер доиграет общее у
/// всех дорожек и не поставит позицию в длительность (живая находка: после смены
/// качества во время клика зритель видел конец фильма).
#[test]
fn a_generation_replaced_mid_stream_ends_cut_short_for_the_player() {
    let mut translator = real_translator();
    let mut inputs: Vec<SessionInput> = recorded_inputs("two-source-buffers-mid-box-cuts")
        .into_iter()
        .filter(|input| {
            !matches!(
                input,
                SessionInput::Message(_, PageSessionMessage::EndOfStream { .. })
            )
        })
        .collect();
    inputs.push(started(1, 2));

    let demuxer = opened_generation_demuxer(run(&mut translator, inputs));

    assert!(matches!(
        demuxer.data_end(),
        media_core::DemuxDataEnd::CutShort {
            playable_end: Some(_)
        }
    ));
}

/// Поколение, которое страница честно закончила (`endOfStream`) до замены, замена не
/// обрывает: его медиа действительно кончилось.
#[test]
fn a_generation_ended_honestly_before_its_replacement_keeps_the_complete_end() {
    let mut translator = real_translator();
    let mut inputs = recorded_inputs("two-source-buffers-mid-box-cuts");
    inputs.push(started(1, 2));

    let demuxer = opened_generation_demuxer(run(&mut translator, inputs));

    assert_eq!(demuxer.data_end(), media_core::DemuxDataEnd::Complete);
}
