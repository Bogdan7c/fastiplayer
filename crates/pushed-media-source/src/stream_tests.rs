//! Тесты границы потока без плеера: отказы, пределы, учёт, порядок вызовов.

use std::num::NonZeroUsize;
use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, Demuxer, TrackKind};

use super::{OpenProgress, PushedStreamDemuxer, PushedStreamOpener, pushed_stream};
use crate::container::PushedContainer;
use crate::error::{LaneLayoutProblem, PushedStreamError, PushedStreamFailureClass};
use crate::lane::{AppendOutcome, LaneId};
use crate::policy::PushedSourcePolicy;
use crate::time::{PushedDuration, TimestampOffset};

/// fMP4 H.264: init — первые 733 байта, затем два фрагмента.
const VIDEO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/video.mp4");
const AUDIO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/audio.mp4");
const VIDEO_INIT_END: usize = 733;
/// Конец первого фрагмента `VIDEO_MP4`.
const VIDEO_FIRST_FRAGMENT_END: usize = 5714;
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Сколько ждать фоновых демуксеров в тестах.
const WAIT: Duration = Duration::from_secs(5);

fn policy() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
}

fn open_within_deadline(
    opener: &mut PushedStreamOpener,
) -> Result<PushedStreamDemuxer, PushedStreamError> {
    let deadline = Instant::now() + WAIT;
    loop {
        match opener.poll_open()? {
            OpenProgress::Ready(demuxer) => return Ok(demuxer),
            OpenProgress::Waiting(hint) => {
                assert!(Instant::now() < deadline, "stream did not open");
                thread::sleep(hint.retry_after());
            }
        }
    }
}

/// Читает события до конца потока или ошибки (пакеты считает по видам).
fn drain(demuxer: &mut PushedStreamDemuxer) -> (usize, usize, Result<(), String>) {
    let deadline = Instant::now() + WAIT;
    let (mut video, mut audio) = (0, 0);
    loop {
        assert!(Instant::now() < deadline, "stream did not finish");
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(packet)) => match packet.kind {
                TrackKind::Video => video += 1,
                TrackKind::Audio => audio += 1,
                TrackKind::Subtitle => {}
            },
            Ok(DemuxReadEvent::EndOfStream) => return (video, audio, Ok(())),
            Ok(DemuxReadEvent::TemporarilyUnavailable(hint)) => thread::sleep(hint.retry_after()),
            Ok(_) => {}
            Err(error) => return (video, audio, Err(format!("{error:#}"))),
        }
    }
}

#[test]
fn separate_video_and_audio_lanes_open_as_one_demuxer_with_both_tracks() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio");
    writer.set_duration(PushedDuration::Finite(Duration::from_millis(400)));
    // Пока байтов нет — ждём, а не ошибка.
    assert!(matches!(opener.poll_open(), Ok(OpenProgress::Waiting(_))));
    writer.append(VIDEO, VIDEO_MP4).expect("video bytes");
    writer.append(AUDIO, AUDIO_MP4).expect("audio bytes");
    writer.end_of_stream().expect("end");

    let mut demuxer = open_within_deadline(&mut opener).expect("opens");
    let kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|track| track.kind).collect();
    assert_eq!(kinds, [TrackKind::Video, TrackKind::Audio]);
    assert_eq!(demuxer.duration(), Some(Duration::from_millis(400)));
    assert!(!matches!(
        demuxer.seekability(),
        media_core::DemuxSeekability::Seekable
    ));
    assert_eq!(drain(&mut demuxer), (10, 20, Ok(())));
    // Всё прочитано — поток ничего не держит в памяти.
    assert_eq!(writer.held_bytes(), 0);
}

#[test]
fn protected_initialization_is_refused_and_poisons_the_stream() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    let mut encrypted = VIDEO_MP4.to_vec();
    let entry = encrypted[28..VIDEO_INIT_END]
        .windows(4)
        .position(|window| window == b"avc1")
        .expect("sample entry")
        + 28;
    encrypted[entry..entry + 4].copy_from_slice(b"encv");

    let error = writer.append(VIDEO, &encrypted).expect_err("protected");
    assert_eq!(error, PushedStreamError::ProtectedContent { lane: VIDEO });
    assert_eq!(
        error.failure_class(),
        PushedStreamFailureClass::ProtectedContent
    );
    // Дальше поток мёртв для обеих сторон — с той же первой причиной.
    assert_eq!(writer.append(VIDEO, VIDEO_MP4), Err(error.clone()));
    assert_eq!(writer.failure(), Some(error.clone()));
    assert!(matches!(opener.poll_open(), Err(failure) if failure == error));
}

#[test]
fn garbage_inside_a_fragment_is_a_stream_error_not_a_panic() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    // Структура боксов цела, но содержимое `moof` испорчено: сканер его пропустит,
    // а демуксер обязан отказать — ошибкой потока, без паники процесса.
    let mut broken = VIDEO_MP4.to_vec();
    for byte in &mut broken[VIDEO_INIT_END + 8..VIDEO_INIT_END + 120] {
        *byte = 0xFF;
    }
    writer
        .append(VIDEO, &broken)
        .expect("framing accepts whole boxes");
    writer.end_of_stream().expect("end");

    let outcome = open_within_deadline(&mut opener).map(|mut demuxer| drain(&mut demuxer));
    let failure = writer.failure().expect("stream failed");
    assert_eq!(
        failure.failure_class(),
        PushedStreamFailureClass::UnparsableMedia
    );
    match outcome {
        Err(error) => assert_eq!(error, failure),
        Ok((_, _, result)) => assert!(result.is_err(), "demuxer must report the broken fragment"),
    }
}

#[test]
fn wrong_container_bytes_are_a_framing_error() {
    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer.add_lane(VIDEO, PushedContainer::WebM).expect("lane");
    let error = writer.append(VIDEO, VIDEO_MP4).expect_err("not WebM");
    assert!(
        matches!(error, PushedStreamError::Framing { lane: VIDEO, .. }),
        "{error:?}"
    );
    assert_eq!(
        error.failure_class(),
        PushedStreamFailureClass::UnparsableMedia
    );
}

#[test]
fn intake_ceiling_is_an_error_instead_of_memory_growth() {
    let small = policy().with_max_held_stream_bytes(NonZeroUsize::new(4 * 1024).expect("non-zero"));
    let (mut writer, _opener) = pushed_stream(small).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    // Никто не читает: init + первый фрагмент (5714 байтов) больше потолка 4 КиБ.
    let error = writer
        .append(VIDEO, VIDEO_MP4)
        .expect_err("over the ceiling");
    assert_eq!(
        error,
        PushedStreamError::IntakeLimitExceeded { limit: 4 * 1024 }
    );
    assert_eq!(error.failure_class(), PushedStreamFailureClass::InputMisuse);
    assert!(writer.held_bytes() <= 4 * 1024 + VIDEO_MP4.len());
}

#[test]
fn oversized_media_segment_is_refused_by_its_header() {
    let small = policy().with_max_media_segment_bytes(NonZeroUsize::new(1024).expect("non-zero"));
    let (mut writer, _opener) = pushed_stream(small).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    let error = writer
        .append(VIDEO, VIDEO_MP4)
        .expect_err("segment too large");
    assert!(
        matches!(error, PushedStreamError::Framing { .. }),
        "{error:?}"
    );
}

#[test]
fn call_order_violations_are_typed_and_first_failure_wins() {
    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    assert_eq!(
        writer.append(VIDEO, VIDEO_MP4),
        Err(PushedStreamError::UnknownLane { lane: VIDEO })
    );
    // Первая ошибка уже убила поток: следующий вызов отвечает ею же.
    assert_eq!(
        writer.add_lane(VIDEO, PushedContainer::FragmentedMp4),
        Err(PushedStreamError::UnknownLane { lane: VIDEO })
    );

    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    assert_eq!(
        writer.add_lane(VIDEO, PushedContainer::FragmentedMp4),
        Err(PushedStreamError::LaneAlreadyDeclared { lane: VIDEO })
    );

    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    assert_eq!(
        writer.append(VIDEO, &VIDEO_MP4[VIDEO_INIT_END..]),
        Err(PushedStreamError::MediaBeforeInitialization { lane: VIDEO })
    );

    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer.add_lane(VIDEO, PushedContainer::MpegTs).expect("ts");
    assert_eq!(
        writer.set_timestamp_offset(VIDEO, TimestampOffset::from_micros(1)),
        Err(PushedStreamError::TimestampOffsetUnsupported { lane: VIDEO })
    );

    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer.end_of_stream().expect("end");
    assert_eq!(
        writer.append(VIDEO, VIDEO_MP4),
        Err(PushedStreamError::InputAfterEnd)
    );
}

#[test]
fn too_many_lanes_are_refused() {
    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    for index in 0..8 {
        writer
            .add_lane(LaneId::new(index), PushedContainer::FragmentedMp4)
            .expect("within limit");
    }
    assert_eq!(
        writer.add_lane(LaneId::new(8), PushedContainer::FragmentedMp4),
        Err(PushedStreamError::TooManyLanes { limit: 8 })
    );
}

#[test]
fn lane_added_after_open_is_refused_and_empty_lanes_are_ignored() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio, never fed");
    writer.append(VIDEO, VIDEO_MP4).expect("video bytes");
    writer.end_of_stream().expect("end");
    // Звуковая полоса кончилась пустой: поток открывается одним видео.
    let mut demuxer = open_within_deadline(&mut opener).expect("opens");
    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(drain(&mut demuxer).0, 10);
    // Поток отдан плееру: новые полосы — ошибка.
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer.append(VIDEO, VIDEO_MP4).expect("bytes");
    let _demuxer = open_within_deadline(&mut opener).expect("opens");
    assert_eq!(
        writer.add_lane(AUDIO, PushedContainer::FragmentedMp4),
        Err(PushedStreamError::LayoutFrozen { lane: AUDIO })
    );
}

#[test]
fn stream_without_media_and_two_video_lanes_are_unsupported_layouts() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer.end_of_stream().expect("end");
    assert_eq!(
        open_within_deadline(&mut opener).err(),
        Some(PushedStreamError::UnsupportedLayout {
            problem: LaneLayoutProblem::NoMedia
        })
    );

    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("first");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("second");
    writer.append(VIDEO, VIDEO_MP4).expect("bytes");
    writer.append(AUDIO, VIDEO_MP4).expect("bytes");
    let error = open_within_deadline(&mut opener)
        .err()
        .expect("two video lanes");
    assert_eq!(
        error,
        PushedStreamError::UnsupportedLayout {
            problem: LaneLayoutProblem::NotOneVideoAndOneAudio
        }
    );
    assert_eq!(writer.failure(), Some(error));
}

#[test]
fn closed_player_makes_further_bytes_discarded_and_releases_memory() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer
        .append(VIDEO, &VIDEO_MP4[..VIDEO_FIRST_FRAGMENT_END])
        .expect("bytes");
    let demuxer = open_within_deadline(&mut opener).expect("opens");
    drop(demuxer);
    assert_eq!(
        writer.append(VIDEO, &VIDEO_MP4[VIDEO_FIRST_FRAGMENT_END..]),
        Ok(AppendOutcome::DiscardedConsumerClosed)
    );
    // Эпизоды выброшены вместе с плеером: в памяти остались только байты сканера.
    let deadline = Instant::now() + WAIT;
    while writer.held_bytes() > 0 {
        assert!(
            Instant::now() < deadline,
            "held {} bytes",
            writer.held_bytes()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn incomplete_segment_at_end_of_stream_is_reported() {
    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer
        .append(VIDEO, &VIDEO_MP4[..VIDEO_INIT_END + 100])
        .expect("bytes");
    let report = writer.end_of_stream().expect("end");
    assert_eq!(report.dropped_incomplete_bytes, 100);
}

#[test]
fn source_leaving_without_end_of_stream_lets_the_player_finish_what_arrived() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer.append(VIDEO, VIDEO_MP4).expect("bytes");
    let mut demuxer = open_within_deadline(&mut opener).expect("opens");
    // Канал оборвался: писатель уничтожен без `endOfStream`.
    drop(writer);
    // Плеер получает всё принятое и обычный конец потока, а не вечное ожидание.
    assert_eq!(drain(&mut demuxer), (10, 0, Ok(())));
}

#[test]
fn frames_depending_on_a_frame_dropped_below_zero_wait_for_the_next_keyframe() {
    // Ключевые кадры фикстуры — 0 и 200 мс. Смещение −100 мс уводит в минус
    // кадры 0, 40, 80; кадры 120 и 160 ссылаются на выброшенный ключевой.
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer
        .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(-100_000))
        .expect("offset");
    writer.append(VIDEO, VIDEO_MP4).expect("bytes");
    writer.end_of_stream().expect("end");
    let mut demuxer = open_within_deadline(&mut opener).expect("opens");

    let deadline = Instant::now() + WAIT;
    let mut packets = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "stream did not finish");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::Packet(packet) => packets.push((packet.pts, packet.keyframe)),
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            _ => {}
        }
    }
    let times: Vec<Duration> = packets.iter().map(|(pts, _)| *pts).collect();
    assert_eq!(
        times,
        [100, 140, 180, 220, 260]
            .map(Duration::from_millis)
            .to_vec()
    );
    assert_eq!(packets[0].1, media_core::PacketKeyframe::Keyframe);
}

#[test]
fn page_flooding_new_episodes_hits_the_pending_episode_limit() {
    let (mut writer, _opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer
        .append(VIDEO, &VIDEO_MP4[..VIDEO_FIRST_FRAGMENT_END])
        .expect("first episode");
    // Каждая смена смещения + медиасегмент — новый эпизод; плеер их не забирает.
    let fragments = [
        &VIDEO_MP4[VIDEO_FIRST_FRAGMENT_END..],
        &VIDEO_MP4[VIDEO_INIT_END..VIDEO_FIRST_FRAGMENT_END],
    ];
    for episode in 1..4_i64 {
        writer
            .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(episode * 1_000_000))
            .expect("offset");
        writer
            .append(
                VIDEO,
                fragments[usize::try_from(episode).expect("small") % 2],
            )
            .expect("episode within the limit");
    }
    writer
        .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(9_000_000))
        .expect("offset");
    let error = writer
        .append(VIDEO, fragments[0])
        .expect_err("fifth episode");
    assert_eq!(
        error,
        PushedStreamError::TooManyPendingEpisodes {
            lane: VIDEO,
            limit: 4
        }
    );
}

#[test]
fn opening_twice_is_refused_without_breaking_the_stream() {
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video");
    writer.append(VIDEO, VIDEO_MP4).expect("bytes");
    writer.end_of_stream().expect("end");
    let mut demuxer = open_within_deadline(&mut opener).expect("opens");
    assert!(matches!(
        opener.poll_open(),
        Err(PushedStreamError::AlreadyOpened)
    ));
    assert_eq!(writer.failure(), None);
    assert_eq!(drain(&mut demuxer), (10, 0, Ok(())));
}
