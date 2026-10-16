// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Функциональные проверки встроенных субтитров на маленьких сгенерированных ffmpeg
//! файлах (`tests/fixtures/subtitles/README.md`): дорожки, метки, пакеты, вложения, seek.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{
    DemuxReadEvent, Demuxer, MediaAttachments, Packet, SkippedAttachmentReason, TrackId, TrackInfo,
    TrackKind, TrackLanguage, subtitle_codec,
};
use source_core::LocalFileSource;
use symphonia_demux::{AttachmentReadLimits, DemuxerOptions, SymphoniaDemuxer};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/subtitles")
        .join(name)
}

fn open(name: &str) -> SymphoniaDemuxer {
    SymphoniaDemuxer::from_file(&fixture(name)).expect("fixture must open")
}

fn open_with_limits(name: &str, limits: AttachmentReadLimits) -> SymphoniaDemuxer {
    let options = DemuxerOptions::default().with_attachment_read_limits(limits);
    SymphoniaDemuxer::from_file_with_options(&fixture(name), options).expect("fixture must open")
}

/// Читает все пакеты до конца потока.
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

fn subtitle_packets(packets: &[Packet], track_id: TrackId) -> Vec<&Packet> {
    packets
        .iter()
        .filter(|packet| packet.kind == TrackKind::Subtitle && packet.track_id == track_id)
        .collect()
}

fn track_of_kind(demuxer: &SymphoniaDemuxer, kind: TrackKind, nth: usize) -> TrackInfo {
    demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == kind)
        .nth(nth)
        .cloned()
        .unwrap_or_else(|| panic!("track {kind:?}#{nth} must exist"))
}

fn text(packet: &Packet) -> &str {
    std::str::from_utf8(&packet.data).expect("subtitle payload is UTF-8")
}

#[test]
fn mkv_publishes_video_audio_and_two_subtitle_tracks_with_labels() {
    let demuxer = open("two-srt-tracks.mkv");

    let kinds: Vec<_> = demuxer.tracks().iter().map(|track| track.kind).collect();
    assert_eq!(
        kinds,
        [
            TrackKind::Video,
            TrackKind::Audio,
            TrackKind::Subtitle,
            TrackKind::Subtitle
        ]
    );

    let video = track_of_kind(&demuxer, TrackKind::Video, 0);
    assert_eq!(video.labels.language, Some(TrackLanguage::new("eng")));
    assert_eq!(video.labels.title.as_deref(), Some("Main video"));

    let audio = track_of_kind(&demuxer, TrackKind::Audio, 0);
    assert_eq!(audio.labels.language, Some(TrackLanguage::new("jpn")));
    assert_eq!(audio.labels.title.as_deref(), Some("Japanese mono"));

    let english = track_of_kind(&demuxer, TrackKind::Subtitle, 0);
    assert_eq!(english.codec_id, subtitle_codec::TEXT_UTF8);
    assert_eq!(english.labels.language, Some(TrackLanguage::new("eng")));
    assert_eq!(english.labels.title.as_deref(), Some("English"));
    assert!(english.labels.disposition.is_default);
    assert!(!english.labels.disposition.is_forced);
    assert!(!english.labels.disposition.is_hearing_impaired);

    let russian = track_of_kind(&demuxer, TrackKind::Subtitle, 1);
    assert_eq!(russian.codec_id, subtitle_codec::TEXT_UTF8);
    assert_eq!(russian.labels.language, Some(TrackLanguage::new("rus")));
    assert_eq!(russian.labels.title.as_deref(), Some("Russian SDH"));
    assert!(!russian.labels.disposition.is_default);
    assert!(russian.labels.disposition.is_forced);
    assert!(russian.labels.disposition.is_hearing_impaired);
}

#[test]
fn mkv_subtitle_packets_carry_text_time_and_duration_next_to_video_and_audio() {
    let mut demuxer = open("two-srt-tracks.mkv");
    let english = track_of_kind(&demuxer, TrackKind::Subtitle, 0).id;
    let russian = track_of_kind(&demuxer, TrackKind::Subtitle, 1).id;

    let packets = read_all_packets(&mut demuxer);

    let english_cues: Vec<_> = subtitle_packets(&packets, english)
        .iter()
        .map(|packet| (packet.pts, packet.duration, text(packet).to_owned()))
        .collect();
    assert_eq!(
        english_cues,
        [
            (
                Duration::from_millis(500),
                Some(Duration::from_millis(1_000)),
                "Hello".to_owned()
            ),
            (
                Duration::from_millis(2_000),
                Some(Duration::from_millis(800)),
                "Second line".to_owned()
            ),
        ]
    );
    let russian_texts: Vec<_> = subtitle_packets(&packets, russian)
        .iter()
        .map(|packet| text(packet).to_owned())
        .collect();
    assert_eq!(russian_texts, ["Привет", "Второй"]);

    // Субтитры не вытеснили остальное: видео и аудио по-прежнему приходят.
    assert!(
        packets
            .iter()
            .filter(|packet| packet.kind == TrackKind::Video)
            .count()
            >= 20
    );
    assert!(packets.iter().any(|packet| packet.kind == TrackKind::Audio));
}

#[test]
fn mkv_subtitle_packets_are_available_through_the_byte_source_route_too() {
    // Приложение открывает локальные файлы через ByteSource, а не через File.
    let source = LocalFileSource::open(fixture("two-srt-tracks.mkv")).expect("open source");
    let mut demuxer = SymphoniaDemuxer::from_byte_source_with_options(
        source,
        "mkv",
        "fixture",
        DemuxerOptions::default(),
    )
    .expect("byte source must open");
    let english = track_of_kind(&demuxer, TrackKind::Subtitle, 0);
    assert_eq!(english.labels.title.as_deref(), Some("English"));

    let packets = read_all_packets(&mut demuxer);

    assert_eq!(subtitle_packets(&packets, english.id).len(), 2);
}

#[test]
fn seeking_back_replays_every_subtitle_cue_of_every_track() {
    let mut demuxer = open("two-srt-tracks.mkv");
    let subtitle_ids = [
        track_of_kind(&demuxer, TrackKind::Subtitle, 0).id,
        track_of_kind(&demuxer, TrackKind::Subtitle, 1).id,
    ];
    let first_pass = read_all_packets(&mut demuxer);

    demuxer.seek(Duration::ZERO).expect("seek to start");
    let second_pass = read_all_packets(&mut demuxer);

    for track_id in subtitle_ids {
        let first: Vec<_> = subtitle_packets(&first_pass, track_id)
            .iter()
            .map(|packet| (packet.pts, text(packet).to_owned()))
            .collect();
        let second: Vec<_> = subtitle_packets(&second_pass, track_id)
            .iter()
            .map(|packet| (packet.pts, text(packet).to_owned()))
            .collect();
        assert_eq!(first.len(), 2, "track {track_id}");
        assert_eq!(second, first, "track {track_id} after seek to start");
    }
}

#[test]
fn seeking_forward_still_delivers_later_cues_of_all_subtitle_tracks() {
    let mut demuxer = open("two-srt-tracks.mkv");
    let subtitle_ids = [
        track_of_kind(&demuxer, TrackKind::Subtitle, 0).id,
        track_of_kind(&demuxer, TrackKind::Subtitle, 1).id,
    ];

    demuxer
        .seek(Duration::from_millis(1_800))
        .expect("seek near the second cue");
    let packets = read_all_packets(&mut demuxer);

    for track_id in subtitle_ids {
        assert!(
            subtitle_packets(&packets, track_id)
                .iter()
                .any(|packet| packet.pts == Duration::from_millis(2_000)),
            "track {track_id} must deliver the cue at 2.0 s after seek"
        );
    }
}

#[test]
fn mp4_mov_text_track_has_language_from_mdhd_and_tx3g_packets() {
    let mut demuxer = open("mov-text-rus.mp4");
    let subtitle = track_of_kind(&demuxer, TrackKind::Subtitle, 0);
    assert_eq!(subtitle.codec_id, subtitle_codec::MOV_TEXT);
    assert_eq!(subtitle.labels.language, Some(TrackLanguage::new("rus")));
    // MP4 не даёт названия и флагов: берутся только язык.
    assert_eq!(subtitle.labels.title, None);
    assert!(!subtitle.labels.disposition.is_forced);
    // Видео-дорожка без языка в файле (ffmpeg пишет `und`) не получает языка.
    let video = track_of_kind(&demuxer, TrackKind::Video, 0);
    assert_eq!(video.labels.language, None);

    let packets = read_all_packets(&mut demuxer);
    let samples = subtitle_packets(&packets, subtitle.id);

    // ffmpeg заполняет паузы между репликами пустыми tx3g-сэмплами (только длина 0):
    // они приходят как обычные пакеты, различать их должен разбор реплики.
    let timeline: Vec<_> = samples
        .iter()
        .map(|packet| (packet.pts, packet.duration, packet.data.len() == 2))
        .collect();
    assert_eq!(
        timeline,
        [
            (Duration::ZERO, Some(Duration::from_millis(500)), true),
            (
                Duration::from_millis(500),
                Some(Duration::from_millis(1_000)),
                false
            ),
            (
                Duration::from_millis(1_500),
                Some(Duration::from_millis(500)),
                true
            ),
            (
                Duration::from_millis(2_000),
                Some(Duration::from_millis(800)),
                false
            ),
            // У последнего сэмпла контейнер не указал длительность.
            (Duration::from_millis(2_800), None, true),
        ]
    );
    // tx3g: 2 байта длины (big endian) + UTF-8 текст.
    for (sample, expected) in [(samples[1], "Привет"), (samples[3], "Второй")] {
        let expected_text = expected.as_bytes();
        assert_eq!(
            &sample.data[..2],
            &(expected_text.len() as u16).to_be_bytes()
        );
        assert_eq!(&sample.data[2..], expected_text);
    }
}

#[test]
fn ass_track_exposes_script_header_as_codec_private_and_event_line_payload() {
    let mut demuxer = open("ass-with-font.mkv");
    let ass = track_of_kind(&demuxer, TrackKind::Subtitle, 0);
    assert_eq!(ass.codec_id, subtitle_codec::ASS);
    assert_eq!(ass.labels.title.as_deref(), Some("Styled"));
    let header = String::from_utf8(ass.codec_private.expect("ASS header").to_vec()).unwrap();
    assert!(header.contains("[Script Info]"), "{header}");
    assert!(
        header.contains("Style: Default,Noto Sans Hebrew"),
        "{header}"
    );

    let packets = read_all_packets(&mut demuxer);
    let cues = subtitle_packets(&packets, ass.id);

    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].pts, Duration::from_millis(500));
    // Формат MKV: ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text.
    assert_eq!(text(cues[0]), "0,0,Default,,0,0,0,,Styled {\\b1}line{\\b0}");
}

#[test]
fn font_attachment_is_returned_once_with_exact_bytes_name_and_media_type() {
    let mut demuxer = open("ass-with-font.mkv");

    let attachments = demuxer.take_attachments();

    assert!(attachments.skipped.is_empty());
    assert_eq!(attachments.attachments.len(), 1);
    let font = &attachments.attachments[0];
    assert_eq!(font.file_name, "NotoSansHebrew-Regular.ttf");
    assert_eq!(font.media_type.as_deref(), Some("font/ttf"));
    let original = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../subtitle-raster/fonts/NotoSansHebrew-Regular.ttf"),
    )
    .expect("font file");
    assert_eq!(&font.bytes[..], &original[..]);
    // Второй вызов не отдаёт те же байты повторно.
    assert!(demuxer.take_attachments().is_empty());
}

#[test]
fn attachment_over_per_file_limit_is_skipped_but_playback_packets_still_flow() {
    let font_size = fs::metadata(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../subtitle-raster/fonts/NotoSansHebrew-Regular.ttf"),
    )
    .expect("font metadata")
    .len();
    let limits = AttachmentReadLimits::default().with_max_bytes_per_attachment(1_024);
    let mut demuxer = open_with_limits("ass-with-font.mkv", limits);

    let attachments = demuxer.take_attachments();

    assert!(attachments.attachments.is_empty());
    assert_eq!(attachments.skipped.len(), 1);
    assert_eq!(
        attachments.skipped[0].file_name,
        "NotoSansHebrew-Regular.ttf"
    );
    assert_eq!(
        attachments.skipped[0].reason,
        SkippedAttachmentReason::TooLarge {
            size_bytes: font_size,
            limit_bytes: 1_024
        }
    );
    // Пропуск вложения не ломает чтение: видео и реплика на месте.
    let ass = track_of_kind(&demuxer, TrackKind::Subtitle, 0);
    let packets = read_all_packets(&mut demuxer);
    assert_eq!(subtitle_packets(&packets, ass.id).len(), 1);
    assert!(packets.iter().any(|packet| packet.kind == TrackKind::Video));
}

#[test]
fn attachment_over_total_limit_is_skipped_with_total_limit_reason() {
    let limits = AttachmentReadLimits::default().with_max_total_bytes(2_048);
    let mut demuxer = open_with_limits("ass-with-font.mkv", limits);

    let MediaAttachments {
        attachments,
        skipped,
    } = demuxer.take_attachments();

    assert!(attachments.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(matches!(
        skipped[0].reason,
        SkippedAttachmentReason::TotalLimitExceeded {
            accepted_total_bytes: 0,
            limit_bytes: 2_048,
            ..
        }
    ));
}

#[test]
fn stream_route_applies_attachment_limits_too() {
    let file = File::open(fixture("ass-with-font.mkv")).expect("open fixture");
    let options = DemuxerOptions::default().with_attachment_read_limits(
        AttachmentReadLimits::default().with_max_bytes_per_attachment(16),
    );

    let mut demuxer = SymphoniaDemuxer::from_stream_with_options(file, "mkv", "fixture", options)
        .expect("stream must open");
    let attachments = demuxer.take_attachments();

    assert!(attachments.attachments.is_empty());
    assert_eq!(attachments.skipped.len(), 1);
}

#[test]
fn files_without_attachments_report_none() {
    let mut demuxer = open("mov-text-rus.mp4");

    assert!(demuxer.take_attachments().is_empty());
}
