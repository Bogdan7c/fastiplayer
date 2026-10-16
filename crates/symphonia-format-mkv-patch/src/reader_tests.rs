// Fastiplayer: тесты Name дорожки и лимитов вложений на реальном (минимальном) Matroska-потоке.

use std::io::Cursor;

use symphonia_core::formats::TrackFlags;
use symphonia_core::formats::prelude::*;
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::RawValue;

use crate::{
    AttachmentReadLimits, FASTIPLAYER_TRACK_NAME_TAG, MkvReader, SkippedAttachmentReason,
    with_attachment_read_limits,
};

/// Кодирует EBML-элемент: id + 8-байтный size vint + payload.
fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = id.to_vec();
    bytes.push(0x01);
    bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
    bytes.extend_from_slice(payload);
    bytes
}

fn uint(id: &[u8], value: u64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

fn text(id: &[u8], value: &str) -> Vec<u8> {
    element(id, value.as_bytes())
}

fn attached_file(uid: u64, name: &str, data: &[u8]) -> Vec<u8> {
    let payload = [
        uint(&[0x46, 0xAE], uid),
        text(&[0x46, 0x6E], name),
        text(&[0x46, 0x60], "font/ttf"),
        element(&[0x46, 0x5C], data),
    ]
    .concat();
    element(&[0x61, 0xA7], &payload)
}

/// Минимальный Matroska: одна subtitle-дорожка с названием, два вложения, пустой кластер.
fn minimal_matroska(small: &[u8], big: &[u8]) -> Vec<u8> {
    matroska_with_track_extra(small, big, &[])
}

/// То же, но с дополнительными дочерними элементами `TrackEntry`.
fn matroska_with_track_extra(small: &[u8], big: &[u8], track_extra: &[u8]) -> Vec<u8> {
    let ebml = element(&[0x1A, 0x45, 0xDF, 0xA3], &text(&[0x42, 0x82], "matroska"));
    let info = element(
        &[0x15, 0x49, 0xA9, 0x66],
        &[
            uint(&[0x2A, 0xD7, 0xB1], 1_000_000),
            text(&[0x4D, 0x80], "test"),
            text(&[0x57, 0x41], "test"),
        ]
        .concat(),
    );
    let track_entry = element(
        &[0xAE],
        &[
            uint(&[0xD7], 1),
            uint(&[0x73, 0xC5], 1),
            uint(&[0x83], 17),
            text(&[0x86], "S_TEXT/UTF8"),
            text(&[0x53, 0x6E], "English SDH"),
            text(&[0x22, 0xB5, 0x9C], "eng"),
            track_extra.to_vec(),
        ]
        .concat(),
    );
    let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &track_entry);
    let attachments = element(
        &[0x19, 0x41, 0xA4, 0x69],
        &[attached_file(1, "small.ttf", small), attached_file(2, "big.ttf", big)].concat(),
    );
    let cluster = element(&[0x1F, 0x43, 0xB6, 0x75], &uint(&[0xE7], 0));
    let segment = element(&[0x18, 0x53, 0x80, 0x67], &[info, tracks, attachments, cluster].concat());
    [ebml, segment].concat()
}

fn open(bytes: Vec<u8>, limits: AttachmentReadLimits) -> MkvReader<'static> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    MkvReader::try_new_with_attachment_limits(source, FormatOptions::default(), limits)
        .expect("minimal matroska must open")
}

fn file_names(reader: &MkvReader<'_>) -> Vec<String> {
    reader
        .attachments()
        .iter()
        .filter_map(|attachment| match attachment {
            Attachment::File(file) => Some(file.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn unlimited_reader_keeps_all_attachments_with_data() {
    let reader = open(minimal_matroska(b"abc", &[7; 1000]), AttachmentReadLimits::UNLIMITED);

    assert_eq!(file_names(&reader), ["small.ttf", "big.ttf"]);
    assert!(reader.skipped_attachments().is_empty());
}

#[test]
fn oversized_attachment_is_skipped_with_name_and_size_while_tracks_survive() {
    let mut reader = open(minimal_matroska(b"abc", &[7; 1000]), AttachmentReadLimits::new(16, 1024));

    assert_eq!(file_names(&reader), ["small.ttf"]);
    let skipped = reader.skipped_attachments();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].file_name, "big.ttf");
    assert_eq!(
        skipped[0].reason,
        SkippedAttachmentReason::TooLarge { size_bytes: 1000, limit_bytes: 16 }
    );
    // Пропуск вложения не ломает разбор остального контейнера.
    assert_eq!(reader.tracks().len(), 1);
    assert!(reader.metadata().current().is_some());
}

#[test]
fn total_limit_skips_attachment_that_would_exceed_it() {
    let reader = open(minimal_matroska(b"abc", &[7; 10]), AttachmentReadLimits::new(100, 12));

    assert_eq!(file_names(&reader), ["small.ttf"]);
    assert_eq!(
        reader.skipped_attachments()[0].reason,
        SkippedAttachmentReason::TotalLimitExceeded {
            size_bytes: 10,
            accepted_total_bytes: 3,
            limit_bytes: 12,
        }
    );
}

#[test]
fn scope_limits_reach_reader_created_through_try_new() {
    let source = MediaSourceStream::new(
        Box::new(Cursor::new(minimal_matroska(b"abc", &[7; 1000]))),
        Default::default(),
    );

    let (reader, skipped) = with_attachment_read_limits(AttachmentReadLimits::new(16, 1024), || {
        MkvReader::try_new(source, FormatOptions::default()).expect("must open")
    });

    assert_eq!(file_names(&reader), ["small.ttf"]);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].file_name, "big.ttf");
}

#[test]
fn track_name_is_published_as_per_track_tag_keyed_by_track_number() {
    let mut reader = open(minimal_matroska(b"a", b"b"), AttachmentReadLimits::UNLIMITED);

    let metadata = reader.metadata();
    let revision = metadata.current().expect("track name revision");
    let per_track = revision.per_track.iter().find(|entry| entry.track_id == 1).expect("track 1");
    let tag = per_track
        .metadata
        .tags
        .iter()
        .find(|tag| tag.raw.key == FASTIPLAYER_TRACK_NAME_TAG)
        .expect("name tag");
    assert_eq!(tag.raw.value.to_string(), "English SDH");
    assert!(matches!(tag.raw.value, RawValue::String(_)));
}

#[test]
fn absent_flag_default_means_default_track_and_explicit_zero_clears_it() {
    let absent = open(minimal_matroska(b"a", b"b"), AttachmentReadLimits::UNLIMITED);
    assert!(absent.tracks()[0].flags.contains(TrackFlags::DEFAULT));

    let explicit_zero = open(
        matroska_with_track_extra(b"a", b"b", &uint(&[0x88], 0)),
        AttachmentReadLimits::UNLIMITED,
    );
    assert!(!explicit_zero.tracks()[0].flags.contains(TrackFlags::DEFAULT));

    let explicit_one = open(
        matroska_with_track_extra(b"a", b"b", &uint(&[0x88], 1)),
        AttachmentReadLimits::UNLIMITED,
    );
    assert!(explicit_one.tracks()[0].flags.contains(TrackFlags::DEFAULT));
}
