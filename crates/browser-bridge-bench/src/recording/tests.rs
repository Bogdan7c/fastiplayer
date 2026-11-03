//! Герметичные тесты разбора, сборки и синтетического генератора записи.

use serde_json::json;

use super::format::{MAGIC, fnv1a_32_hex};
use super::*;
use crate::media::MediaCatalog;

/// Файл с заданными событиями и складом `payload` (манифест собирается руками, а не билдером,
/// чтобы тесты могли подсунуть любые поля).
fn file_with(events: serde_json::Value, store: &[u8]) -> Vec<u8> {
    file_with_manifest(
        json!({
            "format": "fastiplayer-page-stream-recording",
            "version": 1,
            "browser": "firefox",
            "origin": "https://example.test",
            "started_at_unix_ms": 1_793_700_000_000_u64,
            "truncated": false,
            "events": events,
        }),
        store,
    )
}

fn file_with_manifest(manifest: serde_json::Value, store: &[u8]) -> Vec<u8> {
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest encodes");
    let mut file = MAGIC.to_vec();
    file.extend_from_slice(&(manifest_bytes.len() as u32).to_le_bytes());
    file.extend_from_slice(&manifest_bytes);
    file.extend_from_slice(store);
    file
}

fn error_text(bytes: &[u8]) -> String {
    Recording::parse(bytes)
        .expect_err("the recording must be rejected")
        .to_string()
}

#[test]
fn builder_output_is_read_back_with_payload_bytes_and_handoff_time() {
    let mut builder = RecordingBuilder::new("chrome", "https://www.example.test");
    builder.event(0.0, "main", "media_source_created", json!({ "ms": "ms-1" }));
    builder.append(10.5, "main", "ms-1", "sb-1", b"first");
    builder.append(20.0, "worker-1", "ms-1", "sb-1", b"second!");
    builder.event(30.0, "main", "handoff_began", json!({ "el": "el-1" }));
    let recording = Recording::parse(&builder.finish().expect("builds")).expect("parses");

    assert_eq!(recording.manifest.browser, "chrome");
    assert_eq!(recording.manifest.origin, "https://www.example.test");
    assert!(!recording.manifest.truncated);
    assert!(
        recording.warnings().is_empty(),
        "{:?}",
        recording.warnings()
    );
    let appends: Vec<&[u8]> = recording
        .events()
        .iter()
        .filter(|event| event.op == "append")
        .map(|event| recording.payload_of(event))
        .collect();
    assert_eq!(appends, [&b"first"[..], &b"second!"[..]]);
    assert_eq!(recording.handoff_began_at_ms(), Some(30.0));
    assert_eq!(recording.duration_ms(), 30.0);
    assert!(recording.uses_worker_context());
    assert_eq!(recording.store_length(), 12);
}

#[test]
fn header_problems_are_reported_with_their_reason() {
    assert!(error_text(b"FPREC0").contains("too short"));
    let mut wrong_magic = file_with(json!([]), b"");
    wrong_magic[7] = b'9';
    assert!(error_text(&wrong_magic).contains("bad magic"));
    let mut overlong = file_with(json!([]), b"");
    overlong[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(error_text(&overlong).contains("manifest length"));
    let mut not_json = MAGIC.to_vec();
    not_json.extend_from_slice(&4_u32.to_le_bytes());
    not_json.extend_from_slice(b"nope");
    assert!(error_text(&not_json).contains("not valid JSON"));
}

#[test]
fn foreign_format_and_future_version_are_rejected() {
    let manifest = |format: &str, version: u64| {
        json!({
            "format": format, "version": version, "browser": "firefox", "origin": "https://x.test",
            "started_at_unix_ms": 1, "truncated": false, "events": [],
        })
    };
    let foreign = file_with_manifest(manifest("something-else", 1), b"");
    assert!(error_text(&foreign).contains("unsupported recording format 'something-else'"));
    let future = file_with_manifest(manifest("fastiplayer-page-stream-recording", 2), b"");
    assert!(error_text(&future).contains("unsupported recording version 2"));
}

#[test]
fn unknown_op_and_unknown_fields_are_skipped_with_a_warning() {
    let bytes = file_with(
        json!([
            { "t_ms": 1.0, "ctx": "main", "op": "media_source_created", "ms": "ms-1", "extra": 7 },
            { "t_ms": 2.0, "ctx": "main", "op": "teleport", "where": "moon" },
            { "t_ms": 3.0, "ctx": "main", "op": "source_open", "ms": "ms-1" },
        ]),
        b"",
    );
    let recording = Recording::parse(&bytes).expect("future fields do not break reading");
    // Неизвестная операция отброшена, известные остались.
    let ops: Vec<&str> = recording
        .events()
        .iter()
        .map(|event| event.op.as_str())
        .collect();
    assert_eq!(ops, ["media_source_created", "source_open"]);
    let warnings = recording.warnings().join("\n");
    assert!(warnings.contains("unknown op 'teleport'"), "{warnings}");
    assert!(warnings.contains("unknown field 'extra'"), "{warnings}");
}

#[test]
fn missing_required_field_is_an_error_naming_event_op_and_field() {
    let bytes = file_with(
        json!([
            { "t_ms": 1.0, "ctx": "main", "op": "add_source_buffer", "ms": "ms-1", "sb": "sb-1" },
        ]),
        b"",
    );
    let message = error_text(&bytes);
    assert!(message.contains("event #0"), "{message}");
    assert!(message.contains("add_source_buffer"), "{message}");
    assert!(message.contains("'mime'"), "{message}");
    let no_time = file_with(
        json!([{ "ctx": "main", "op": "source_open", "ms": "ms-1" }]),
        b"",
    );
    assert!(error_text(&no_time).contains("'t_ms'"));
}

#[test]
fn append_payload_must_fit_the_store_and_match_its_hash() {
    let append = |offset: u64, length: u64, hash: &str| {
        json!([{
            "t_ms": 1.0, "ctx": "main", "op": "append", "ms": "ms-1", "sb": "sb-1",
            "offset": offset, "length": length, "hash": hash,
        }])
    };
    let store = b"hello";
    let good_hash = fnv1a_32_hex(store);
    assert!(Recording::parse(&file_with(append(0, 5, &good_hash), store)).is_ok());
    assert!(error_text(&file_with(append(3, 5, &good_hash), store)).contains("outside"));
    assert!(error_text(&file_with(append(0, 5, "deadbeef"), store)).contains("hash mismatch"));
    let wrong_type = file_with(
        json!([{ "t_ms": 1.0, "ctx": "main", "op": "append", "ms": "m", "sb": "s",
                 "offset": "0", "length": 5, "hash": good_hash }]),
        store,
    );
    assert!(error_text(&wrong_type).contains("'offset' must be"));
}

#[test]
fn fnv1a_matches_the_published_test_vectors() {
    // Эталонные значения FNV-1a 32: пустая строка и "a".
    assert_eq!(fnv1a_32_hex(b""), "811c9dc5");
    assert_eq!(fnv1a_32_hex(b"a"), "e40c292c");
}

#[test]
fn events_out_of_time_order_produce_a_warning() {
    let bytes = file_with(
        json!([
            { "t_ms": 5.0, "ctx": "main", "op": "source_open", "ms": "ms-1" },
            { "t_ms": 1.0, "ctx": "main", "op": "source_ended", "ms": "ms-1" },
        ]),
        b"",
    );
    let recording = Recording::parse(&bytes).expect("parses");
    assert!(
        recording
            .warnings()
            .iter()
            .any(|text| text.contains("back in time"))
    );
}

#[test]
fn a_recording_from_the_main_thread_only_has_no_worker_context() {
    let bytes = file_with(
        json!([{ "t_ms": 1.0, "ctx": "main", "op": "source_open", "ms": "ms-1" }]),
        b"",
    );
    let recording = Recording::parse(&bytes).expect("parses");
    assert!(!recording.uses_worker_context());
    assert_eq!(recording.handoff_began_at_ms(), None);
}

/// Крошечный каталог на диске: один видео- и один аудио-дорожка по два сегмента.
fn tiny_catalog(directory: &std::path::Path) -> MediaCatalog {
    let files = [
        ("a/video-init.mp4", &b"VINIT"[..]),
        ("a/video-1.m4s", b"V1V1"),
        ("a/video-2.m4s", b"V2"),
        ("a/audio-init.mp4", b"AINIT"),
        ("a/audio-1.m4s", b"A1"),
        ("a/audio-2.m4s", b"A2A2A2"),
    ];
    for (name, content) in files {
        let path = directory.join(name);
        std::fs::create_dir_all(path.parent().expect("has parent")).expect("mkdir");
        std::fs::write(path, content).expect("write file");
    }
    let track = |id: &str, kind: &str, mime: &str, prefix: &str| {
        json!({
            "id": id, "kind": kind, "mime": mime, "init": format!("a/{prefix}-init.mp4"),
            "segments": [
                { "url": format!("a/{prefix}-1.m4s"), "start_s": 0.0, "duration_s": 2.0 },
                { "url": format!("a/{prefix}-2.m4s"), "start_s": 2.0, "duration_s": 2.0 },
            ],
        })
    };
    let catalog = json!({
        "version": 1,
        "assets": { "clip": {
            "kind": "segmented", "duration_s": 4.0,
            "tracks": [
                track("video", "video", "video/mp4; codecs=\"avc1.64001e\"", "video"),
                track("audio", "audio", "audio/mp4; codecs=\"mp4a.40.2\"", "audio"),
            ],
        }},
    });
    std::fs::write(directory.join("catalog.json"), catalog.to_string()).expect("catalog");
    MediaCatalog::load(directory).expect("catalog loads")
}

#[test]
fn synthetic_recording_replays_the_asset_bytes_in_a_valid_timeline() {
    let directory = tempfile::tempdir().expect("tempdir");
    let catalog = tiny_catalog(directory.path());
    let options = SynthesisOptions {
        handoff_at_ms: 1500.0,
        buffer_ahead_s: 1.0,
    };
    let bytes = synthesize_from_asset(&catalog, "clip", options).expect("synthesizes");
    let recording = Recording::parse(&bytes).expect("the generated file is a valid recording");

    assert!(
        recording.warnings().is_empty(),
        "{:?}",
        recording.warnings()
    );
    let ops: Vec<&str> = recording
        .events()
        .iter()
        .map(|event| event.op.as_str())
        .collect();
    assert_eq!(
        ops.iter().filter(|op| **op == "add_source_buffer").count(),
        2
    );
    assert_eq!(ops.last(), Some(&"end_of_stream"));
    // Передача внутри записи, а не за её концом.
    let handoff = recording.handoff_began_at_ms().expect("handoff event");
    assert_eq!(handoff, 1500.0);
    assert!(recording.duration_ms() > handoff);
    // Байты склада: init видео и звука, затем сегменты по времени, видео перед звуком при равенстве.
    let payloads: Vec<&[u8]> = recording
        .events()
        .iter()
        .filter(|event| event.op == "append")
        .map(|event| recording.payload_of(event))
        .collect();
    assert_eq!(
        payloads,
        [&b"VINIT"[..], b"AINIT", b"V1V1", b"A1", b"V2", b"A2A2A2"]
    );
    // События идут по времени, а `update_end` следует за каждым `append`.
    let times: Vec<f64> = recording.events().iter().map(|event| event.t_ms).collect();
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    assert_eq!(ops.iter().filter(|op| **op == "update_end").count(), 6);
}

#[test]
fn synthesis_rejects_unknown_and_non_segmented_assets_with_a_reason() {
    let directory = tempfile::tempdir().expect("tempdir");
    let catalog = tiny_catalog(directory.path());
    let missing = synthesize_from_asset(&catalog, "no-such", SynthesisOptions::default())
        .expect_err("unknown asset");
    assert!(
        missing.to_string().contains("not in the catalog"),
        "{missing}"
    );
}
