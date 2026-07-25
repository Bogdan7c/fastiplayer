//! Focused traceability S36A для exact ISM/MSS base/VOD H.264/AAC profile.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

/// Относительный путь единственного owner-а machine-readable compatibility profile.
const PROFILE_PATH: &str = "compatibility/2026.07.04/profile.json";

/// Загружает S00 profile без чтения либо изменения upstream evidence fixtures.
fn load_profile() -> Value {
    // Cargo предоставляет стабильный absolute crate root независимо от cwd теста.
    let profile_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PROFILE_PATH);
    // Ошибка чтения содержит только checked-in путь и не раскрывает fixture payload.
    let profile_bytes = fs::read(&profile_path)
        .unwrap_or_else(|error| panic!("не удалось прочитать {profile_path:?}: {error}"));
    // Malformed checked-in JSON является немедленной ошибкой traceability.
    serde_json::from_slice(&profile_bytes)
        .unwrap_or_else(|error| panic!("не удалось разобрать {profile_path:?}: {error}"))
}

/// Возвращает обязательную строку либо завершает тест точной schema-ошибкой.
fn required_string<'value>(value: &'value Value, field: &str) -> &'value str {
    // S00 machine contract не допускает отсутствующую либо нестроковую identity.
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("обязательное строковое поле `{field}` отсутствует"))
}

/// Возвращает обязательный JSON array либо завершает тест точной schema-ошибкой.
fn required_array<'value>(value: &'value Value, field: &str) -> &'value [Value] {
    // S00 machine contract не допускает отсутствующий либо не-array collection.
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("обязательное array-поле `{field}` отсутствует"))
}

/// S36A сужает aggregate ISM promise до exact base/VOD H.264/AAC evidence.
#[test]
fn manifest_splits_ism_base_profile_without_live_promotion() {
    // Загружаем только machine-readable profile; fixture traceability проверяет общий S00 test.
    let profile = load_profile();
    // Codec profiles являются единственным owner-ом exact family sets.
    let codec_profiles = required_array(&profile, "codec_profiles");
    // Находим узкий video profile для единственной approved ISM Target row.
    let ism_video_profile = codec_profiles
        .iter()
        .find(|codec_profile| required_string(codec_profile, "id") == "ism-base-video-h264")
        .unwrap_or_else(|| panic!("exact ISM H.264 codec profile отсутствует"));
    // Fixture доказывает H.264 и не разрешает соседние video families.
    assert_eq!(required_array(ism_video_profile, "families"), ["h264"]);
    // Находим узкий audio profile для единственной approved ISM Target row.
    let ism_audio_profile = codec_profiles
        .iter()
        .find(|codec_profile| required_string(codec_profile, "id") == "ism-base-audio-aac")
        .unwrap_or_else(|| panic!("exact ISM AAC codec profile отсутствует"));
    // Fixture доказывает AAC и не разрешает соседние audio families.
    assert_eq!(required_array(ism_audio_profile, "families"), ["aac"]);
    // Находим explicit provisional set остальных существующих video families.
    let provisional_video_profile = codec_profiles
        .iter()
        .find(|codec_profile| {
            required_string(codec_profile, "id") == "ism-provisional-other-existing-video"
        })
        .unwrap_or_else(|| panic!("provisional ISM video codec profile отсутствует"));
    // Set является exact complement H.264 внутри existing major-web-video profile.
    assert_eq!(
        required_array(provisional_video_profile, "families"),
        ["vp8", "vp9", "av1", "h265"]
    );
    // Находим explicit provisional set остальных существующих audio families.
    let provisional_audio_profile = codec_profiles
        .iter()
        .find(|codec_profile| {
            required_string(codec_profile, "id") == "ism-provisional-other-existing-audio"
        })
        .unwrap_or_else(|| panic!("provisional ISM audio codec profile отсутствует"));
    // Set является exact complement AAC внутри existing proven-native-audio profile.
    assert_eq!(
        required_array(provisional_audio_profile, "families"),
        [
            "adpcm", "alac", "flac", "mp1", "mp2", "mp3", "pcm", "vorbis", "opus"
        ]
    );

    // Собираем все approved Target rows, относящиеся к exact `ism` transport.
    let ism_target_rows = required_array(&profile, "target_rows")
        .iter()
        .filter(|target_row| target_row.get("transport").and_then(Value::as_str) == Some("ism"))
        .collect::<Vec<_>>();
    // S00 одобряет ровно одну base/VOD row и не материализует live/DVR promise.
    assert_eq!(ism_target_rows.len(), 1);
    // Единственная row получает стабильный narrow identity вместо старого aggregate ID.
    let ism_target_row = ism_target_rows[0];
    // Stable row ID закрепляет exact transport/container/codec scope.
    assert_eq!(
        required_string(ism_target_row, "id"),
        "ism-mss-base-h264-aac-fmp4"
    );
    // Existing fixture identity сохраняется без подмены evidence.
    assert_eq!(
        required_string(ism_target_row, "fixture_id"),
        "target-ism-fmp4"
    );
    // Target использует только два exact codec profiles.
    assert_eq!(
        required_array(ism_target_row, "codec_profile_refs"),
        ["ism-base-video-h264", "ism-base-audio-aac"]
    );
    // Old aggregate row не может сосуществовать с exact split.
    assert!(
        required_array(&profile, "target_rows")
            .iter()
            .all(|target_row| required_string(target_row, "id") != "ism-mss-fmp4"),
        "старый aggregate ISM Target остался после exact split"
    );

    // Excluded rows фиксируют gaps отдельно от approved namespace.
    let excluded_rows = required_array(&profile, "excluded_rows");
    // Проверяем обе codec exclusions через их exact profile references.
    for (excluded_row_id, excluded_profile_id) in [
        (
            "ism-mss-base-other-existing-video-codecs",
            "ism-provisional-other-existing-video",
        ),
        (
            "ism-mss-base-other-existing-audio-codecs",
            "ism-provisional-other-existing-audio",
        ),
    ] {
        // Каждая exclusion обязана существовать как самостоятельная row.
        let excluded_row = excluded_rows
            .iter()
            .find(|row| required_string(row, "id") == excluded_row_id)
            .unwrap_or_else(|| panic!("exact provisional ISM codec exclusion отсутствует"));
        // Provisional status допускает promotion только с будущим evidence.
        assert_eq!(
            required_string(excluded_row, "status"),
            "ProfileExcludedProvisional"
        );
        // Exclusion не может схлопнуть несколько family sets в broad profile.
        assert_eq!(
            required_array(excluded_row, "codec_profile_refs"),
            [excluded_profile_id]
        );
    }
    // Live/DVR gap обязан оставаться explicit provisional exclusion.
    let live_exclusion = excluded_rows
        .iter()
        .find(|row| required_string(row, "id") == "ism-mss-live-dvr")
        .unwrap_or_else(|| panic!("provisional ISM live/DVR exclusion отсутствует"));
    // Отсутствие approved live fixture не превращается в молчаливый Target.
    assert_eq!(
        required_string(live_exclusion, "status"),
        "ProfileExcludedProvisional"
    );
    // Ни одна exact ISM Target identity не пересекается с exclusion namespace.
    let ism_target_ids = ism_target_rows
        .iter()
        .map(|target_row| required_string(target_row, "id"))
        .collect::<HashSet<_>>();
    // Все ISM exclusions обязаны оставаться вне approved Target set.
    assert!(
        excluded_rows
            .iter()
            .filter(|row| row.get("transport").and_then(Value::as_str) == Some("ism"))
            .all(|row| !ism_target_ids.contains(required_string(row, "id"))),
        "ISM row одновременно Target и provisional exclusion"
    );
}
