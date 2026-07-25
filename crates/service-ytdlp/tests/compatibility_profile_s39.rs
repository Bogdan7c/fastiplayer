//! Focused traceability S39 для exact RTMP variant gate.

// Читаем checked-in JSON profile без production parser-а или network side effects.
use serde_json::Value;
// Читаем immutable fixture manifest только внутри hermetic integration test-а.
use std::fs;
// Собираем absolute test path из Cargo-provided crate root.
use std::path::PathBuf;

// Canonical S00 profile остаётся единственным machine-readable owner-ом решения.
const PROFILE_PATH: &str = "compatibility/2026.07.04/profile.json";

/// Загружает canonical S00 profile для focused S39 assertions.
fn load_profile() -> Value {
    // Берём crate root из compile-time Cargo contract, а не из process cwd.
    let profile_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PROFILE_PATH);
    // Ошибка чтения является test infrastructure failure и сохраняет safe local path.
    let profile_bytes = fs::read(&profile_path)
        .unwrap_or_else(|error| panic!("не удалось прочитать {}: {error}", profile_path.display()));
    // Невалидный checked-in JSON обязан немедленно уронить focused test.
    serde_json::from_slice(&profile_bytes)
        .unwrap_or_else(|error| panic!("не удалось разобрать {}: {error}", profile_path.display()))
}

/// Возвращает обязательное строковое поле profile row.
fn required_string<'value>(value: &'value Value, field: &str) -> &'value str {
    // Missing или non-string поле означает schema regression, а не optional evidence.
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("обязательное строковое поле `{field}` отсутствует"))
}

/// Возвращает обязательный array из profile document.
fn required_array<'value>(value: &'value Value, field: &str) -> &'value [Value] {
    // Missing или non-array поле не может молча превратиться в пустой evidence set.
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("обязательный array `{field}` отсутствует"))
}

/// Находит одну exact row по stable ID.
fn required_row_by_id<'rows>(rows: &'rows [Value], expected_id: &str) -> &'rows Value {
    // Stable ID отделяет exact wire decision от соседних variant-ов.
    rows.iter()
        .find(|row| required_string(row, "id") == expected_id)
        .unwrap_or_else(|| panic!("обязательная S39 row `{expected_id}` отсутствует"))
}

/// S39 не повышает identity-only RTMP inventory до недоказанного wire provider-а.
#[test]
fn exact_rtmp_variants_remain_excluded_without_wire_fixtures() {
    // Загружаем только canonical S00 evidence; production provider здесь не создаётся.
    let profile = load_profile();
    // Target rows описывают approved future implementation scope.
    let target_rows = required_array(&profile, "target_rows");
    // Aggregate row сохраняет upstream serialized identity inventory.
    let aggregate_row = required_row_by_id(target_rows, "rtmp-family-flv");
    // Transport name явно запрещает трактовать metadata fixture как wire approval.
    assert_eq!(
        required_string(aggregate_row, "transport"),
        "rtmp_rtmpe_or_rtmp_ffmpeg_identity_only"
    );
    // Existing fixture остаётся traceable, но не меняет identity-only semantics.
    assert_eq!(
        required_string(aggregate_row, "fixture_id"),
        "target-rtmp-flv"
    );
    // Protocol inventory хранит upstream identities, а не взаимозаменяемые wire aliases.
    let rtmp_identity_inventory = required_array(&profile, "protocol_aliases")
        .iter()
        .find(|family| required_string(family, "family") == "rtmp")
        .unwrap_or_else(|| panic!("S00 RTMP identity inventory отсутствует"));
    // Inventory не включает TLS/tunnel variants и не даёт им implicit admission.
    assert_eq!(
        required_array(rtmp_identity_inventory, "aliases"),
        ["rtmp", "rtmpe", "rtmp_ffmpeg"]
    );

    // Ни один exact variant не получает самостоятельную approved Target row.
    for exact_variant in ["rtmp", "rtmpe", "rtmp_ffmpeg", "rtmps", "rtmpt", "rtmpte"] {
        // Aggregate inventory нельзя использовать как alias для exact wire transport.
        assert!(
            target_rows
                .iter()
                .all(|row| row.get("transport").and_then(Value::as_str) != Some(exact_variant)),
            "exact RTMP variant `{exact_variant}` ошибочно повышен до Target"
        );
    }

    // Exclusion namespace является authoritative S39 no-op evidence.
    let excluded_rows = required_array(&profile, "excluded_rows");
    // Plain RTMP ждёт собственный deterministic handshake/chunk/message/play fixture.
    let plain_rtmp = required_row_by_id(excluded_rows, "rtmp-plain-wire");
    // Будущее evidence может повысить variant отдельным profile extension.
    assert_eq!(
        required_string(plain_rtmp, "status"),
        "ProfileExcludedProvisional"
    );
    // Exact transport identity не схлопывается с encrypted/tunneled variants.
    assert_eq!(required_string(plain_rtmp, "transport"), "rtmp");
    // Причина exclusion требует именно полного deterministic wire proof.
    assert_eq!(
        required_string(plain_rtmp, "reason"),
        "identity_only_metadata_is_not_a_deterministic_handshake_chunk_message_play_wire_fixture"
    );

    // RTMPE требует отдельного настоящего crypto evidence.
    let encrypted_rtmp = required_row_by_id(excluded_rows, "rtmpe-encrypted-wire");
    // Отсутствие encrypted fixture сохраняет provisional exclusion.
    assert_eq!(
        required_string(encrypted_rtmp, "status"),
        "ProfileExcludedProvisional"
    );
    // Exact encrypted wire identity остаётся отличной от plain RTMP.
    assert_eq!(required_string(encrypted_rtmp, "transport"), "rtmpe");
    // Metadata-only fixture не подменяет crypto handshake и encrypted payload.
    assert_eq!(
        required_string(encrypted_rtmp, "reason"),
        "no_deterministic_rtmpe_crypto_handshake_and_encrypted_payload_fixture"
    );

    // Extractor downloader identity никогда не становится wire provider-ом.
    let ffmpeg_identity = required_row_by_id(excluded_rows, "rtmp-ffmpeg-pseudo-protocol");
    // Non-wire identity является жёстким exclusion, а не будущим alias.
    assert_eq!(
        required_string(ffmpeg_identity, "status"),
        "ProfileExcluded"
    );
    // Exact identity фиксирует запрет hidden FFmpeg fallback.
    assert_eq!(required_string(ffmpeg_identity, "transport"), "rtmp_ffmpeg");
    // Причина exclusion запрещает превращать extractor identity в subprocess fallback.
    assert_eq!(
        required_string(ffmpeg_identity, "reason"),
        "extractor_downloader_identity_is_not_a_wire_protocol_and_has_no_hidden_ffmpeg_fallback"
    );

    // TLS и tunnel variants требуют независимых future wire fixtures.
    for (excluded_id, exact_transport) in [
        ("rtmps-tls-wire", "rtmps"),
        ("rtmpt-http-tunnel-wire", "rtmpt"),
        ("rtmpte-encrypted-http-tunnel-wire", "rtmpte"),
    ] {
        // Каждая variant row обязана существовать отдельно.
        let excluded_variant = required_row_by_id(excluded_rows, excluded_id);
        // Отдельное future evidence может повысить только эту exact row.
        assert_eq!(
            required_string(excluded_variant, "status"),
            "ProfileExcludedProvisional"
        );
        // Variant не нормализуется в plain RTMP.
        assert_eq!(
            required_string(excluded_variant, "transport"),
            exact_transport
        );
    }
}
