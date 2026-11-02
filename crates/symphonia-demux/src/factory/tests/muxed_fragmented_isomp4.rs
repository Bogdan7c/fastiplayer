//! fMP4 «видео + звук в одном фрагменте» на входе без перемотки (сессия 09a моста).
//!
//! В `mdat` каждого фрагмента ffmpeg кладёт сначала всё видео (~80 КиБ), потом весь
//! звук, а читатель отдаёт кадры по времени вперемешку — и возвращается внутри `mdat`
//! назад дальше окна `MediaSourceStream` (64 КиБ). Вход «только вперёд» (поток байтов,
//! упорядоченные сегменты HLS/MSE) раньше падал «out-of-bounds seek for a non-seekable
//! stream» уже на втором кадре видео; локальный файл (перемотка) — нет. Тест сверяет:
//! без перемотки приходят те же пакеты в том же порядке, что из перематываемого файла.
//!
//! Фикстура общая с источником «сегменты извне» (там же её `generate.sh`): это ровно
//! тот вид медиа, на котором поломку нашёл стенд моста.

use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget, OrderedSegmentKind};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use source_core::CancellationToken;

use super::SymphoniaDemuxFactory;
use super::fragmented_isomp4::open_local;
use super::ordered_segments::{open_ordered, segment};
use crate::DemuxerOptions;

/// Кадров видео в фикстуре (1,92 с при 25 к/с).
const VIDEO_PACKETS: usize = 48;
/// Предел событий: защита от вечного цикла при поломке.
const MAX_EVENTS: usize = 1_000;

/// Пакет без деталей, которые зависят от вида входа (смещение в байтах есть не везде).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PacketDigest {
    kind: TrackKind,
    pts: Duration,
    dts: Option<Duration>,
    data: Bytes,
}

/// Байты фикстуры muxed fMP4 с длинными прогонами дорожек.
fn muxed_long_runs_fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pushed-media-source/fixtures/muxed-h264-aac-long-runs.mp4");
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Режет fMP4 по верхним боксам: init (`ftyp`+`moov`) и фрагменты (`moof`+`mdat`).
fn split_init_and_fragments(file: &[u8]) -> (Bytes, Vec<Bytes>) {
    let mut position = 0;
    let mut init_end = 0;
    let mut fragment_starts = Vec::new();
    while position < file.len() {
        let size = u32::from_be_bytes(file[position..position + 4].try_into().expect("size"));
        let size = usize::try_from(size).expect("box size");
        assert!(size >= 8, "a top-level box has a header");
        match &file[position + 4..position + 8] {
            b"ftyp" | b"moov" => init_end = position + size,
            b"moof" => fragment_starts.push(position),
            b"mdat" => {}
            other => panic!("unexpected top-level box {other:?}"),
        }
        position += size;
    }
    let mut fragment_ends = fragment_starts[1..].to_vec();
    fragment_ends.push(file.len());
    let fragments = fragment_starts
        .iter()
        .zip(fragment_ends)
        .map(|(&start, end)| Bytes::copy_from_slice(&file[start..end]))
        .collect();
    (Bytes::copy_from_slice(&file[..init_end]), fragments)
}

/// Читает все пакеты до конца потока; ошибка чтения — падение теста с причиной.
fn read_every_packet(demuxer: &mut dyn Demuxer, input: &str) -> Vec<PacketDigest> {
    let mut packets = Vec::new();
    for _ in 0..MAX_EVENTS {
        match demuxer
            .next_event()
            .unwrap_or_else(|error| panic!("{input}: reading failed: {error:#}"))
        {
            DemuxReadEvent::Packet(packet) => packets.push(PacketDigest {
                kind: packet.kind,
                pts: packet.pts,
                dts: packet.dts,
                data: packet.data,
            }),
            DemuxReadEvent::EndOfStream => return packets,
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("{input}: a finite input does not wait")
            }
        }
    }
    panic!("{input}: no end of stream after {MAX_EVENTS} events");
}

/// Вход «поток байтов без перемотки» через тот же реестр, что у приложения.
fn open_forward_only_stream(file: Vec<u8>) -> Box<dyn Demuxer + Send> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory"),
        ))
        .expect("register Symphonia");
    registry
        .open(
            DemuxInput::byte_stream(Box::new(Cursor::new(file))),
            DemuxHints::none(),
            DemuxSniffBudget::new(
                NonZeroUsize::new(4_096).expect("sniff bytes"),
                NonZeroUsize::MIN,
                Duration::from_secs(1),
            )
            .expect("sniff budget"),
            CancellationToken::never_cancelled(),
        )
        .expect("open forward-only muxed fMP4")
}

/// Эталон: пакеты из перематываемого локального файла.
fn packets_from_the_seekable_file(file: &[u8]) -> Vec<PacketDigest> {
    let (_file_guard, mut demuxer) = open_local(file);
    read_every_packet(demuxer.as_mut(), "seekable file")
}

#[test]
fn the_seekable_reference_holds_every_frame_of_both_tracks() {
    let reference = packets_from_the_seekable_file(&muxed_long_runs_fixture());

    let video = reference
        .iter()
        .filter(|packet| packet.kind == TrackKind::Video)
        .count();
    let audio = reference
        .iter()
        .filter(|packet| packet.kind == TrackKind::Audio)
        .count();
    assert_eq!(video, VIDEO_PACKETS);
    assert!(audio >= 90, "audio packets: {audio}");
    // Кадры дорожек идут вперемешку по времени, а не прогонами из `mdat`.
    let first_audio = reference
        .iter()
        .position(|packet| packet.kind == TrackKind::Audio)
        .expect("audio packets");
    assert!(first_audio <= 2, "audio starts at packet {first_audio}");
}

#[test]
fn a_forward_only_byte_stream_yields_the_same_packets_as_the_seekable_file() {
    let file = muxed_long_runs_fixture();
    let reference = packets_from_the_seekable_file(&file);

    let mut demuxer = open_forward_only_stream(file);
    let packets = read_every_packet(demuxer.as_mut(), "forward-only stream");

    assert_eq!(packets.len(), reference.len());
    assert!(
        packets == reference,
        "forward-only packets differ from the file"
    );
}

#[test]
fn ordered_init_and_media_segments_yield_the_same_packets_as_the_seekable_file() {
    let file = muxed_long_runs_fixture();
    let reference = packets_from_the_seekable_file(&file);
    let (init, fragments) = split_init_and_fragments(&file);
    assert_eq!(fragments.len(), 3, "the fixture has three fragments");
    let mut segments = vec![segment(1, OrderedSegmentKind::Initialization, init)];
    segments.extend(
        fragments
            .into_iter()
            .zip(2..)
            .map(|(fragment, sequence)| segment(sequence, OrderedSegmentKind::Media, fragment)),
    );

    let mut demuxer = open_ordered(segments, CancellationToken::never_cancelled(), None)
        .expect("open ordered muxed fMP4");
    let packets = read_every_packet(demuxer.as_mut(), "ordered segments");

    assert_eq!(packets.len(), reference.len());
    assert!(
        packets == reference,
        "ordered-segment packets differ from the file"
    );
}

#[test]
fn a_forward_only_stream_cut_inside_a_fragment_plays_what_arrived_then_fails_structurally() {
    let file = muxed_long_runs_fixture();
    let reference = packets_from_the_seekable_file(&file);
    let (init, fragments) = split_init_and_fragments(&file);
    // Обрыв посреди звука первого фрагмента: всё видео фрагмента пришло, звук — нет.
    let first_fragment = &fragments[0];
    let cut_at = init.len() + first_fragment.len() - 1_000;
    let mut demuxer = open_forward_only_stream(file[..cut_at].to_vec());

    let mut delivered = Vec::new();
    let error = loop {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(packet)) => delivered.push(PacketDigest {
                kind: packet.kind,
                pts: packet.pts,
                dts: packet.dts,
                data: packet.data,
            }),
            Ok(DemuxReadEvent::EndOfStream) => panic!("a cut mdat is not a clean end"),
            Ok(_) => {}
            Err(error) => break error,
        }
        assert!(delivered.len() < MAX_EVENTS, "no error after the cut");
    };

    assert!(
        !delivered.is_empty(),
        "packets before the cut are delivered"
    );
    assert_eq!(
        delivered[..],
        reference[..delivered.len()],
        "delivered packets match the file"
    );
    assert!(
        format!("{error:#}").contains("unexpected end of atom"),
        "structural truncation error: {error:#}"
    );
}
