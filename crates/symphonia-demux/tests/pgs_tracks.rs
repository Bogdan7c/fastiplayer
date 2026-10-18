// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Функциональная проверка PGS: настоящий MKV (`fixtures/subtitles/pgs-basic.mkv`) ->
//! демуксер -> пакеты дорожки `S_HDMV/PGS` -> `PgsTrackDecoder` -> хранилище композиций ->
//! события с интервалами показа и реальными пикселями картинки.

use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{DemuxReadEvent, Demuxer, Packet, TrackKind, subtitle_codec};
use subtitle_core::{PictureForcedFlag, SubtitleContent, SubtitleEvent, SubtitleTime};
use subtitle_pgs::{
    CompositionEnd, PgsCompositionStore, PgsDecoderLimits, PgsTrackDecoder, PictureDecodeLimits,
    decode_picture_rgba,
};
use symphonia_demux::SymphoniaDemuxer;

const DECODER_LIMITS: PgsDecoderLimits = PgsDecoderLimits {
    max_object_rle_bytes: 1 << 20,
    max_epoch_rle_bytes: 4 << 20,
};
const PIXEL_LIMITS: PictureDecodeLimits = PictureDecodeLimits {
    max_picture_pixels: 1 << 20,
};

fn open() -> SymphoniaDemuxer {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/subtitles/pgs-basic.mkv");
    SymphoniaDemuxer::from_file(&path).expect("fixture must open")
}

fn read_all_packets(demuxer: &mut SymphoniaDemuxer) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("read event") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => return packets,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
        }
    }
}

fn time_of(packet: &Packet) -> SubtitleTime {
    SubtitleTime::from_micros(i64::try_from(packet.pts.as_micros()).expect("fits i64"))
}

/// Прогоняет пакеты PGS-дорожки через декодер в хранилище (как это делает приложение).
fn feed_store(decoder: &mut PgsTrackDecoder, store: &mut PgsCompositionStore, packets: &[&Packet]) {
    for packet in packets {
        for composition in decoder.push_block(time_of(packet), &packet.data) {
            store.insert(composition, CompositionEnd::UntilNextDisplaySet);
        }
    }
}

fn close(actual: [u8; 4], expected: [u8; 4]) {
    for channel in 0..4 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= 3,
            "{actual:?} vs {expected:?}"
        );
    }
}

fn picture_of(event: &SubtitleEvent) -> &subtitle_core::SubtitlePictureCue {
    match event.content() {
        SubtitleContent::Picture(cue) => cue,
        other => panic!("expected a picture cue, got {other:?}"),
    }
}

#[test]
fn mkv_exposes_pgs_track_with_its_codec_and_one_block_per_display_set() {
    let mut demuxer = open();
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Subtitle)
        .cloned()
        .expect("subtitle track");
    assert_eq!(track.codec_id, subtitle_codec::HDMV_PGS);

    let packets = read_all_packets(&mut demuxer);
    let pgs: Vec<_> = packets
        .iter()
        .filter(|packet| packet.track_id == track.id)
        .collect();
    let times: Vec<_> = pgs.iter().map(|packet| packet.pts).collect();
    assert_eq!(
        times,
        [
            Duration::from_millis(500),
            Duration::from_millis(1_500),
            Duration::from_millis(2_000),
            Duration::from_millis(2_600)
        ]
    );
    // Блоки без длительности: конец показа определяется следующим набором.
    assert!(pgs.iter().all(|packet| packet.duration.is_none()));
}

#[test]
fn demuxed_pgs_packets_decode_to_timed_pictures_with_expected_pixels() {
    let mut demuxer = open();
    let track_id = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Subtitle)
        .expect("subtitle track")
        .id;
    let packets = read_all_packets(&mut demuxer);
    let pgs: Vec<_> = packets
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .collect();

    let mut decoder = PgsTrackDecoder::new(DECODER_LIMITS);
    let mut store = PgsCompositionStore::new();
    feed_store(&mut decoder, &mut store, &pgs);

    let events = store.events();
    let spans: Vec<_> = events
        .iter()
        .map(|event| (event.start(), event.end()))
        .collect();
    assert_eq!(
        spans,
        [
            (
                SubtitleTime::from_millis(500),
                SubtitleTime::from_millis(1_500)
            ),
            (
                SubtitleTime::from_millis(2_000),
                SubtitleTime::from_millis(2_600)
            ),
        ]
    );
    assert_eq!(decoder.statistics().malformed_segments, 0);
    assert_eq!(
        decoder.statistics().compositions_skipped_missing_resource,
        0
    );

    // Обычная реплика: 16x8 в (4, 40), слева красный, справа зелёный.
    let regular = picture_of(&events[0]);
    assert_eq!(regular.forced(), PictureForcedFlag::Regular);
    assert_eq!((regular.origin().x, regular.origin().y), (4, 40));
    assert_eq!(regular.reference_size().width().get(), 64);
    let image = decode_picture_rgba(regular, PIXEL_LIMITS)
        .expect("decode regular")
        .image;
    assert_eq!((image.width(), image.height()), (16, 8));
    close(image.pixel(0, 0).expect("pixel"), [255, 0, 0, 255]);
    close(image.pixel(7, 7).expect("pixel"), [255, 0, 0, 255]);
    close(image.pixel(8, 0).expect("pixel"), [0, 255, 0, 255]);
    close(image.pixel(15, 7).expect("pixel"), [0, 255, 0, 255]);

    // Forced-реплика: 12x6 в (44, 6), слева синий, справа белый с альфой 128 (премультиплицирован).
    let forced = picture_of(&events[1]);
    assert_eq!(forced.forced(), PictureForcedFlag::Forced);
    assert_eq!((forced.origin().x, forced.origin().y), (44, 6));
    let image = decode_picture_rgba(forced, PIXEL_LIMITS)
        .expect("decode forced")
        .image;
    assert_eq!((image.width(), image.height()), (12, 6));
    close(image.pixel(0, 0).expect("pixel"), [0, 0, 255, 255]);
    close(image.pixel(6, 0).expect("pixel"), [128, 128, 128, 128]);

    // В момент между репликами (после «очистки») ничего не показывается.
    let at_gap = SubtitleTime::from_millis(1_800);
    assert!(events.iter().all(|event| !event.is_active_at(at_gap)));
    assert!(events[0].is_active_at(SubtitleTime::from_millis(1_000)));
}

#[test]
fn seek_back_and_reread_restores_the_same_timeline_without_duplicates() {
    let mut demuxer = open();
    let track_id = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Subtitle)
        .expect("subtitle track")
        .id;
    let mut decoder = PgsTrackDecoder::new(DECODER_LIMITS);
    let mut store = PgsCompositionStore::new();

    let first_pass = read_all_packets(&mut demuxer);
    let first: Vec<_> = first_pass
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .collect();
    feed_store(&mut decoder, &mut store, &first);
    let timeline_before = store.events();

    // Seek на начало: декодер и хранилище получают разрыв, пакеты читаются заново.
    demuxer.seek(Duration::ZERO).expect("seek to start");
    decoder.reset_after_discontinuity();
    store.note_discontinuity();
    let second_pass = read_all_packets(&mut demuxer);
    let second: Vec<_> = second_pass
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .collect();
    feed_store(&mut decoder, &mut store, &second);

    assert_eq!(store.events(), timeline_before);
    assert_eq!(store.len(), 4);
}
