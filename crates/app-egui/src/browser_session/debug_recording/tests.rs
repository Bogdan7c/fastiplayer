//! Тесты «чёрного ящика»: записанное равно присланному, `at_ms` не убывает, ошибка
//! записи выключает запись с одним предупреждением.

use std::path::Path;

use browser_bridge_protocol::SessionKey;
use browser_bridge_session::{SessionInstance, SessionRef, SessionWireTap};

use super::{RECORDING_STOPPED_MESSAGE, StreamRecorder};
use crate::browser_session::test_log_capture::with_captured_log;

fn session(instance: u64) -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab-1-video-1").expect("session key"),
        instance: SessionInstance(instance),
    }
}

/// Единственный каталог сеанса внутри `root`.
fn only_session_directory(root: &Path) -> std::path::PathBuf {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .expect("root is readable")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    entries.remove(0)
}

/// Числа `at_ms` из манифеста по порядку (разбор достаточно прост для формата записи).
fn manifest_at_ms(manifest: &str) -> Vec<u64> {
    manifest
        .split("\"at_ms\":")
        .skip(1)
        .map(|rest| {
            rest.split(|character: char| !character.is_ascii_digit())
                .next()
                .and_then(|digits| digits.parse().ok())
                .expect("at_ms is a number")
        })
        .collect()
}

#[test]
fn recorded_files_equal_the_wire_bytes_and_manifest_keeps_order() {
    let root = tempfile::tempdir().expect("temp dir");
    let recorder = StreamRecorder::new(root.path().to_path_buf());
    let session = session(1);
    // Не-UTF-8 и нулевые байты: рамка записывается как есть, без перекодирования.
    let frame = vec![1_u8, 0, 0, 0, 255, 254, 0, 7];
    let text = br#"{"type":"generation_started","generation":1}"#;

    recorder.page_text(&session, text);
    recorder.page_binary(&session, &frame);
    recorder.player_text(&session, r#"{"type":"clock","position_us":0}"#);
    recorder.page_text(&session, br#"{"type":"tab_gone","reason":"closed"}"#);
    recorder.connection_closed(&session);

    let directory = only_session_directory(root.path());
    assert_eq!(
        directory.file_name().and_then(|n| n.to_str()),
        Some("session-1")
    );
    assert_eq!(std::fs::read(directory.join("001.json")).unwrap(), text);
    assert_eq!(std::fs::read(directory.join("002.bin")).unwrap(), frame);
    assert_eq!(
        std::fs::read(directory.join("003.json")).unwrap(),
        br#"{"type":"tab_gone","reason":"closed"}"#
    );
    let manifest = std::fs::read_to_string(directory.join("stream.json")).unwrap();
    // Порядок шагов манифеста = порядок прихода; исходящее в манифест не попадает.
    let order: Vec<&str> = manifest
        .split('"')
        .filter(|part| part.ends_with(".json") || part.ends_with(".bin"))
        .collect();
    assert_eq!(order, ["001.json", "002.bin", "003.json"], "{manifest}");
    let times = manifest_at_ms(&manifest);
    assert_eq!(times.len(), 3);
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    // Исходящее лежит отдельно, с временем и сообщением как есть.
    let outgoing = std::fs::read_to_string(directory.join("outgoing.jsonl")).unwrap();
    assert_eq!(outgoing.lines().count(), 1);
    assert!(
        outgoing.contains(r#""message":{"type":"clock","position_us":0}}"#),
        "{outgoing}"
    );
}

#[test]
fn each_session_instance_gets_its_own_numbered_directory() {
    let root = tempfile::tempdir().expect("temp dir");
    let recorder = StreamRecorder::new(root.path().to_path_buf());
    recorder.page_text(&session(1), b"{}");
    recorder.page_text(&session(2), b"{}");
    recorder.page_text(&session(1), b"{\"again\":1}");
    assert_eq!(
        std::fs::read(root.path().join("session-1/002.json")).unwrap(),
        b"{\"again\":1}"
    );
    assert_eq!(
        std::fs::read(root.path().join("session-2/001.json")).unwrap(),
        b"{}"
    );
}

#[test]
fn a_new_recorder_does_not_overwrite_an_earlier_recording() {
    let root = tempfile::tempdir().expect("temp dir");
    StreamRecorder::new(root.path().to_path_buf()).page_text(&session(1), b"first");
    StreamRecorder::new(root.path().to_path_buf()).page_text(&session(1), b"second");
    assert_eq!(
        std::fs::read(root.path().join("session-1/001.json")).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::read(root.path().join("session-2/001.json")).unwrap(),
        b"second"
    );
}

#[test]
fn unwritable_root_stops_recording_with_exactly_one_warning() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // От root права не действуют: тогда запись удалась бы, и проверять нечего.
    if std::fs::create_dir(root.path().join("probe")).is_ok() {
        return;
    }
    let recorder = StreamRecorder::new(root.path().to_path_buf());
    let (_, log) = with_captured_log(|| {
        for _ in 0..5 {
            recorder.page_text(&session(1), b"{}");
            recorder.page_binary(&session(1), b"bytes");
        }
        recorder.connection_closed(&session(1));
    });
    assert_eq!(log.matches(RECORDING_STOPPED_MESSAGE).count(), 1, "{log}");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
