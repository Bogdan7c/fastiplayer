//! Склеенные MPEG-TS записи (`cat a.ts b.ts`) и потеря пакетов в записи эфира.
//!
//! На стыке двух независимых ffmpeg-записей continuity counter каждого PID-а
//! начинается заново, а PTS/DTS/PCR второй записи снова стартуют с 1.4 с. Состав
//! программы тот же (PID-ы, кодеки, SPS/PPS), поэтому это «мягкий» разрыв:
//! demuxer не публикует `TracksChanged` (в player-core это seek-подобный полный
//! сброс — теряется звук, видео ждёт keyframe) и продолжает одну монотонную шкалу
//! презентации. Настоящая смена дорожек (другой кодек в PMT) по-прежнему
//! публикует `TracksChanged`.
//!
//! Фикстуры: `tests/fixtures/concatenated/` (`generate.sh`, README).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test helpers fail the test by panicking"
)]

use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::time::Duration;

use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackKind};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};

/// Размер transport packet-а.
const TS_PACKET_BYTES: usize = 188;
/// Кадров видео в каждой части (0.8 с при 10 к/с).
const VIDEO_FRAMES_PER_PART: usize = 8;
/// AC-3 кадров в каждой части (0.8 с = 25 × 1536 сэмплов @ 48 кГц).
const AUDIO_FRAMES_PER_PART: usize = 25;
/// Шаг видео 10 к/с в тиках 90 кГц.
const VIDEO_STEP_90KHZ: i64 = 9_000;
/// Длительность AC-3 кадра (1536 сэмплов @ 48 кГц) в тиках 90 кГц.
const AUDIO_FRAME_90KHZ: i64 = 2_880;
/// Первый audio PTS ffmpeg-записи (muxer сдвигает старт на 1.4 с).
const FIRST_AUDIO_PTS_90KHZ: i64 = 126_000;
/// Первый video PTS ffmpeg-записи (по ffprobe).
const FIRST_VIDEO_PTS_90KHZ: i64 = 126_480;
/// Видео PID ffmpeg (`0x100`) и аудио PID (`0x101`).
const VIDEO_PID: u16 = 0x100;
const AUDIO_PID: u16 = 0x101;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/concatenated")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn part_a() -> Vec<u8> {
    fixture("part-a-h264-ac3-440hz.ts")
}

fn part_b() -> Vec<u8> {
    fixture("part-b-h264-ac3-700hz.ts")
}

fn part_c_aac() -> Vec<u8> {
    fixture("part-c-h264-aac-700hz.ts")
}

/// `cat first second`: ровно то, что делает пользователь со склейкой записей.
fn concatenate(first: &[u8], second: &[u8]) -> Vec<u8> {
    [first, second].concat()
}

fn open_byte_stream(bytes: Vec<u8>) -> MpegTsDemuxer {
    MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open byte stream")
}

/// Seekable вход строит bounded index и потом переигрывает начало: шкала
/// презентации обязана совпасть с последовательным чтением.
fn open_local_file(bytes: &[u8]) -> (tempfile::TempDir, MpegTsDemuxer) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("concatenated.ts");
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open local file");
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open local file");
    (directory, demuxer)
}

/// Все события до EOS (без самого EOS).
fn drain_events(demuxer: &mut MpegTsDemuxer) -> Vec<DemuxReadEvent> {
    let mut events = Vec::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::EndOfStream => return events,
            event => events.push(event),
        }
    }
}

fn track_resets(events: &[DemuxReadEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
        .count()
}

fn packets_of(events: &[DemuxReadEvent], kind: TrackKind) -> Vec<Packet> {
    events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.kind == kind => Some(packet.clone()),
            _ => None,
        })
        .collect()
}

fn pts_90khz(packet: &Packet) -> i64 {
    packet
        .track_pts
        .expect("MPEG-TS packet has track PTS")
        .units
        .get()
}

/// Пакеты одной части, прочитанные отдельно: эталон «ничего не потеряно».
fn standalone_packets(bytes: Vec<u8>, kind: TrackKind) -> Vec<Packet> {
    let mut demuxer = open_byte_stream(bytes);
    packets_of(&drain_events(&mut demuxer), kind)
}

/// Склейка играет как одна запись: без `TracksChanged`, без потерь и с
/// непрерывной шкалой (вторая запись продолжает первую ровно там, где та
/// закончилась, а не прыгает назад к 1.4 с).
fn assert_plays_as_one_recording(events: &[DemuxReadEvent], input: &str) {
    assert_eq!(
        track_resets(events),
        0,
        "{input}: the join of identical programs must not reset tracks"
    );

    let audio = packets_of(events, TrackKind::Audio);
    let video = packets_of(events, TrackKind::Video);
    assert_eq!(
        audio.len(),
        2 * AUDIO_FRAMES_PER_PART,
        "{input}: audio frames"
    );
    assert_eq!(
        video.len(),
        2 * VIDEO_FRAMES_PER_PART,
        "{input}: video frames"
    );

    // Звук: каждый кадр начинается ровно там, где закончился предыдущий, и на
    // стыке тоже — player-core считает аудио-часы по сэмплам, любая дыра или
    // перекрытие здесь стало бы постоянным рассинхроном.
    for (index, packet) in audio.iter().enumerate() {
        assert_eq!(
            pts_90khz(packet),
            FIRST_AUDIO_PTS_90KHZ + index as i64 * AUDIO_FRAME_90KHZ,
            "{input}: audio frame {index} is on one continuous timeline"
        );
    }
    // Видео: та же шкала, ровный шаг кадра через стык, первый кадр второй
    // записи — её IDR (декодер продолжает без ожидания keyframe).
    for (index, packet) in video.iter().enumerate() {
        assert_eq!(
            pts_90khz(packet),
            FIRST_VIDEO_PTS_90KHZ + index as i64 * VIDEO_STEP_90KHZ,
            "{input}: video frame {index} is on one continuous timeline"
        );
    }
    assert!(
        video[VIDEO_FRAMES_PER_PART].keyframe.is_known_keyframe(),
        "{input}: second recording starts with its IDR"
    );

    // Ничего не потеряно и не испорчено: байты каждой части совпадают с
    // отдельным чтением этой части.
    for (kind, packets, per_part) in [
        (TrackKind::Audio, &audio, AUDIO_FRAMES_PER_PART),
        (TrackKind::Video, &video, VIDEO_FRAMES_PER_PART),
    ] {
        let expected: Vec<Vec<u8>> = [part_a(), part_b()]
            .into_iter()
            .flat_map(|part| standalone_packets(part, kind))
            .map(|packet| packet.data.to_vec())
            .collect();
        let actual: Vec<Vec<u8>> = packets.iter().map(|packet| packet.data.to_vec()).collect();
        assert_eq!(expected.len(), 2 * per_part, "{input}: reference {kind:?}");
        assert_eq!(actual, expected, "{input}: {kind:?} payloads of both parts");
    }
}

#[test]
fn concatenated_recordings_play_as_one_timeline_from_byte_stream() {
    let mut demuxer = open_byte_stream(concatenate(&part_a(), &part_b()));
    let events = drain_events(&mut demuxer);
    assert_plays_as_one_recording(&events, "byte stream");
}

#[test]
fn concatenated_recordings_play_as_one_timeline_from_local_file() {
    let (_directory, mut demuxer) = open_local_file(&concatenate(&part_a(), &part_b()));
    let events = drain_events(&mut demuxer);
    assert_plays_as_one_recording(&events, "local file");
}

#[test]
fn seek_into_second_recording_uses_the_continuous_timeline() {
    let bytes = concatenate(&part_a(), &part_b());
    let forward = {
        let mut demuxer = open_byte_stream(bytes.clone());
        drain_events(&mut demuxer)
    };

    let (_directory, mut demuxer) = open_local_file(&bytes);
    // 2.5 с на непрерывной шкале — середина второй записи (она идёт с 2.2 с).
    let target = Duration::from_millis(2_500);
    let result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("seek into the second recording");
    // Ближайший decode point не позже цели — IDR второй записи на 2.2053 с
    // (198480 тиков), а не её «сырые» 1.4 с.
    let second_recording_idr_90khz =
        FIRST_VIDEO_PTS_90KHZ + VIDEO_FRAMES_PER_PART as i64 * VIDEO_STEP_90KHZ;
    assert_eq!(
        result
            .actual_track_timestamp
            .expect("track timestamp")
            .units
            .get(),
        second_recording_idr_90khz
    );

    let tracks_before_seek = demuxer.tracks().to_vec();
    let events = drain_events(&mut demuxer);
    // Seek в локальном файле штатно отдаёт один маркер `TracksChanged` с теми же
    // дорожками до первого пакета (player-core узнаёт в нём сам seek); других
    // сбросов дорожек после seek-а нет.
    let first_packet = events
        .iter()
        .position(|event| matches!(event, DemuxReadEvent::Packet(_)))
        .expect("packets after seek");
    assert!(track_resets(&events[first_packet..]) == 0);
    for event in &events[..first_packet] {
        if let DemuxReadEvent::TracksChanged(update) = event {
            assert_eq!(
                update.tracks, tracks_before_seek,
                "seek marker keeps tracks"
            );
        }
    }
    let video = packets_of(&events, TrackKind::Video);
    assert!(video[0].keyframe.is_known_keyframe());
    assert_eq!(pts_90khz(&video[0]), second_recording_idr_90khz);
    let audio = packets_of(&events, TrackKind::Audio);
    // Seek попал на IDR в начале второй записи: дальше читается ровно она
    // целиком — хвост последовательного чтения с теми же байтами и временем.
    assert_eq!(audio.len(), AUDIO_FRAMES_PER_PART, "second recording audio");
    assert_eq!(video.len(), VIDEO_FRAMES_PER_PART, "second recording video");
    for (kind, after_seek) in [(TrackKind::Audio, &audio), (TrackKind::Video, &video)] {
        let forward_packets = packets_of(&forward, kind);
        let forward_tail = &forward_packets[forward_packets.len() - after_seek.len()..];
        for (expected, actual) in forward_tail.iter().zip(after_seek.iter()) {
            assert_eq!(actual.data, expected.data, "{kind:?} payload after seek");
            assert_eq!(
                pts_90khz(actual),
                pts_90khz(expected),
                "{kind:?}: after seek every packet keeps its forward-playback timestamp"
            );
        }
    }
}

/// Transport packet-ы входа (188 байт каждый).
fn transport_packets(bytes: &[u8]) -> &[[u8; TS_PACKET_BYTES]] {
    bytes.as_chunks::<TS_PACKET_BYTES>().0
}

fn packet_pid(packet: &[u8; TS_PACKET_BYTES]) -> u16 {
    (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
}

fn starts_pes(packet: &[u8; TS_PACKET_BYTES]) -> bool {
    packet[1] & 0x40 != 0
}

/// Индексы transport packet-ов PID-а, которые продолжают PES (без payload_unit_start).
fn continuation_packet_indexes(bytes: &[u8], pid: u16) -> Vec<usize> {
    packet_indexes(bytes, |packet| {
        packet_pid(packet) == pid && !starts_pes(packet)
    })
}

/// Индексы transport packet-ов PID-а, начинающих PES (payload_unit_start).
fn pes_start_packet_indexes(bytes: &[u8], pid: u16) -> Vec<usize> {
    packet_indexes(bytes, |packet| {
        packet_pid(packet) == pid && starts_pes(packet)
    })
}

fn packet_indexes(bytes: &[u8], select: impl Fn(&[u8; TS_PACKET_BYTES]) -> bool) -> Vec<usize> {
    transport_packets(bytes)
        .iter()
        .enumerate()
        .filter(|(_, packet)| select(packet))
        .map(|(index, _)| index)
        .collect()
}

/// Индекс пакета, с которого начался аудио PES, содержащий пакет `index`.
fn audio_pes_start_before(bytes: &[u8], index: usize) -> usize {
    pes_start_packet_indexes(bytes, AUDIO_PID)
        .into_iter()
        .rfind(|&start| start < index)
        .expect("audio PES start before the lost packet")
}

fn without_packets(bytes: &[u8], dropped: &[usize]) -> Vec<u8> {
    transport_packets(bytes)
        .iter()
        .enumerate()
        .filter(|(index, _)| !dropped.contains(index))
        .flat_map(|(_, packet)| packet.iter().copied())
        .collect()
}

#[test]
fn lost_packets_inside_pes_drop_only_damaged_units_without_track_reset() {
    let intact = part_a();
    // Потеря посреди PES: середина видео (между двумя IDR) и середина звука.
    let video_continuations = continuation_packet_indexes(&intact, VIDEO_PID);
    let audio_continuations = continuation_packet_indexes(&intact, AUDIO_PID);
    assert_loss_drops_only_damaged_units(
        &intact,
        video_continuations[video_continuations.len() / 3],
        audio_continuations[audio_continuations.len() / 2],
    );
}

/// Потерян хвост PES: следующий пакет PID-а уже начинает новый PES. Видео PES
/// без объявленной длины мог быть обрезан, аудио PES не добрал объявленную
/// длину — оба отбрасываются, следующие единицы идут как обычно.
#[test]
fn lost_pes_tail_before_next_pes_start_drops_only_that_unit() {
    let intact = part_a();
    let last_continuation_before_pes_start = |pid: u16| {
        let starts = pes_start_packet_indexes(&intact, pid);
        // Середина записи; последний пакет этого PID-а перед стартом PES.
        let next_start = starts[starts.len() / 2];
        continuation_packet_indexes(&intact, pid)
            .into_iter()
            .rfind(|&index| index < next_start)
            .expect("PES tail packet")
    };
    let lost_video = last_continuation_before_pes_start(VIDEO_PID);
    let lost_audio = last_continuation_before_pes_start(AUDIO_PID);
    let packets = transport_packets(&intact);
    assert_eq!(
        packet_pid(&packets[lost_video]),
        VIDEO_PID,
        "fixture layout"
    );
    assert_eq!(
        packet_pid(&packets[lost_audio]),
        AUDIO_PID,
        "fixture layout"
    );
    assert_loss_drops_only_damaged_units(&intact, lost_video, lost_audio);
}

fn assert_loss_drops_only_damaged_units(intact: &[u8], lost_video: usize, lost_audio: usize) {
    let intact_events = {
        let mut demuxer = open_byte_stream(intact.to_vec());
        drain_events(&mut demuxer)
    };
    let intact_audio = packets_of(&intact_events, TrackKind::Audio);
    let intact_video = packets_of(&intact_events, TrackKind::Video);
    let damaged = without_packets(intact, &[lost_video, lost_audio]);

    let mut demuxer = open_byte_stream(damaged);
    let events = drain_events(&mut demuxer);
    assert_eq!(
        track_resets(&events),
        0,
        "packet loss is not a track change"
    );

    // Каждый уцелевший пакет — байт-в-байт тот же и на том же месте шкалы:
    // потеря не сдвигает и не перебазирует время.
    let audio = packets_of(&events, TrackKind::Audio);
    let video = packets_of(&events, TrackKind::Video);
    for packet in audio.iter().chain(video.iter()) {
        let reference = if packet.kind == TrackKind::Audio {
            &intact_audio
        } else {
            &intact_video
        };
        let original = reference
            .iter()
            .find(|candidate| pts_90khz(candidate) == pts_90khz(packet))
            .expect("surviving packet keeps its timestamp");
        assert_eq!(original.data, packet.data, "surviving packet is intact");
    }
    // Пропал ровно повреждённый PES (его кадры несут byte offset его начала),
    // весь остальной звук на месте.
    let damaged_audio_pes_offset =
        (TS_PACKET_BYTES * audio_pes_start_before(intact, lost_audio)) as u64;
    let expected_audio: Vec<i64> = intact_audio
        .iter()
        .filter(|packet| packet.byte_offset != Some(damaged_audio_pes_offset))
        .map(pts_90khz)
        .collect();
    assert!(
        expected_audio.len() < intact_audio.len(),
        "fixture: PES lost"
    );
    assert_eq!(
        audio.iter().map(pts_90khz).collect::<Vec<_>>(),
        expected_audio,
        "only frames of the damaged PES are lost"
    );
    // Видео продолжается сразу после повреждения, не дожидаясь следующего IDR:
    // кадры между повреждённым и следующим keyframe-ом доставлены.
    let damaged_video_index = intact_video
        .iter()
        .position(|candidate| {
            !video
                .iter()
                .any(|packet| pts_90khz(packet) == pts_90khz(candidate))
        })
        .expect("one video access unit is lost");
    let next_frame = &intact_video[damaged_video_index + 1];
    assert!(
        !next_frame.keyframe.is_known_keyframe(),
        "fixture: loss sits between keyframes"
    );
    assert!(
        video
            .iter()
            .any(|packet| pts_90khz(packet) == pts_90khz(next_frame)),
        "the frame right after the damaged one is delivered"
    );
    assert!(video.len() >= VIDEO_FRAMES_PER_PART - 2);
}

#[test]
fn real_program_change_at_join_still_publishes_track_change() {
    let mut demuxer = open_byte_stream(concatenate(&part_a(), &part_c_aac()));
    assert_eq!(
        demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.codec_id.as_str()),
        Some("A_AC3")
    );
    let events = drain_events(&mut demuxer);
    let updates: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::TracksChanged(update) => Some(update.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !updates.is_empty(),
        "AC-3 → AAC in PMT is a real track change"
    );
    let final_audio_codec = updates
        .last()
        .and_then(|update| {
            update
                .tracks
                .iter()
                .find(|track| track.kind == TrackKind::Audio)
                .map(|track| track.codec_id.clone())
        })
        .expect("audio track after the change");
    assert_ne!(final_audio_codec, "A_AC3");

    // Шкала остаётся монотонной и через настоящую смену дорожек: AAC второй
    // записи не прыгает назад к 1.4 с.
    let audio = packets_of(&events, TrackKind::Audio);
    // AC-3 пакет начинается синхрословом 0x0B77; AAC demuxer отдаёт без ADTS заголовка.
    let first_aac = audio
        .iter()
        .position(|packet| !packet.data.starts_with(&[0x0b, 0x77]))
        .expect("AAC packets after the change");
    let last_ac3_end = audio[first_aac - 1]
        .track_pts
        .map(|pts| pts.units.get() + AUDIO_FRAME_90KHZ)
        .expect("AC-3 pts");
    assert_eq!(pts_90khz(&audio[first_aac]), last_ac3_end);
}
