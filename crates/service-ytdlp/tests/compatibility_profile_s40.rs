//! Focused traceability S40 для serializable special-provider expansion gate.

// Используем JSON value, чтобы test проверял canonical checked-in S00 schema без production DTO.
use serde_json::Value;
// Читаем только локальные hermetic evidence files без network или extractor process.
use std::fs;
// Строим путь относительно crate root, а не относительно process working directory.
use std::path::PathBuf;

// Canonical S00 profile остаётся единственным machine-readable owner-ом gate evidence.
const PROFILE_PATH: &str = "compatibility/2026.07.04/profile.json";
// Synthetic exclusion fixture доказывает lossy live-state serialization без реальных секретов.
const EXCLUSION_FIXTURE_PATH: &str =
    "compatibility/2026.07.04/fixtures/official-synthetic/exclusions-and-unknown.json";

/// Загружает обязательный checked-in JSON document для focused S40 assertions.
fn load_json_document(relative_path: &str) -> Value {
    // Берём crate root из compile-time Cargo contract.
    let document_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    // Ошибка чтения является test infrastructure failure и не может стать пустым evidence.
    let document_bytes = fs::read(&document_path).unwrap_or_else(|error| {
        panic!("не удалось прочитать {}: {error}", document_path.display())
    });
    // Невалидный checked-in JSON обязан немедленно уронить gate.
    serde_json::from_slice(&document_bytes)
        .unwrap_or_else(|error| panic!("не удалось разобрать {}: {error}", document_path.display()))
}

/// Возвращает обязательное строковое поле evidence row.
fn required_string<'value>(value: &'value Value, field: &str) -> &'value str {
    // Missing или non-string поле означает schema regression.
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("обязательное строковое поле `{field}` отсутствует"))
}

/// Возвращает обязательный JSON array из evidence document.
fn required_array<'value>(value: &'value Value, field: &str) -> &'value [Value] {
    // Missing или non-array поле не может молча превратиться в доказанный empty set.
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("обязательный array `{field}` отсутствует"))
}

/// Находит обязательную row по stable string identity.
fn required_row_by_field<'value>(
    rows: &'value [Value],
    identity_field: &str,
    expected_identity: &str,
) -> &'value Value {
    // Поиск остаётся exact и не нормализует provider identities.
    rows.iter()
        .find(|row| required_string(row, identity_field) == expected_identity)
        .unwrap_or_else(|| {
            panic!("обязательная row `{expected_identity}` по полю `{identity_field}` отсутствует")
        })
}

/// S40 остаётся no-op, пока S00 не содержит отдельной public-serializable special row.
#[test]
fn special_provider_gate_has_no_approved_rows_or_cards() {
    // Загружаем canonical S00 profile без production provider construction.
    let profile = load_json_document(PROFILE_PATH);
    // Target rows являются единственным approved future implementation inventory.
    let target_rows = required_array(&profile, "target_rows");
    // Пустой общий inventory сделал бы no-op proof тривиальным и недостоверным.
    assert!(
        !target_rows.is_empty(),
        "S00 target inventory неожиданно пуст"
    );

    // Проверяем direct ownership каждой существующей target row.
    for target_row in target_rows {
        // Каждая approved row обязана иметь concrete future sessions.
        let future_sessions = required_array(target_row, "future_sessions");
        // Empty ownership запрещён базовым S00 contract и здесь проверяется повторно.
        assert!(
            !future_sessions.is_empty(),
            "target row не имеет concrete future owner"
        );
        // Ни одна текущая row не должна неявно создавать generic S40 provider.
        for future_session in future_sessions {
            // Session identity обязана быть строкой.
            let future_session = future_session
                .as_str()
                .unwrap_or_else(|| panic!("future session identity обязана быть строкой"));
            // S40 gate не является provider implementation owner-ом.
            assert_ne!(future_session, "S40");
            // Отдельная S40P card допустима только после новой exact S00 row.
            assert!(
                !future_session.starts_with("S40P-"),
                "S00 неожиданно содержит необсуждённую S40P card `{future_session}`"
            );
        }
    }

    // Exact special alias family хранит exclusions, а не generic provider aliases.
    let special_alias_family = required_row_by_field(
        required_array(&profile, "protocol_aliases"),
        "family",
        "special_private_state_excluded",
    );
    // Exact identities нельзя схлопывать в один fake transport.
    assert_eq!(
        required_array(special_alias_family, "aliases"),
        [
            "bunnycdn",
            "soopvod",
            "niconico_live",
            "fc2_live",
            "websocket_frag"
        ]
    );

    // WebSocket object state остаётся явно несериализуемой runtime semantics.
    let websocket_state = required_row_by_field(
        required_array(&profile, "format_fields"),
        "path",
        "downloader_options.ws",
    );
    // JSON repr не повышает live object до воспроизводимого descriptor-а.
    assert_eq!(
        required_string(websocket_state, "classification"),
        "RequiresLiveExtractorState"
    );
    // Profile disposition остаётся hard exclusion.
    assert_eq!(
        required_string(websocket_state, "profile_disposition"),
        "ProfileExcluded"
    );

    // Generator fragments также требуют живого extractor state.
    let generator_fragments = required_row_by_field(
        required_array(&profile, "format_fields"),
        "path",
        "fragments(generator_or_repr)",
    );
    // Serializable concrete fragments и lossy generator repr остаются разными rows.
    assert_eq!(
        required_string(generator_fragments, "classification"),
        "RequiresLiveExtractorState"
    );

    // Private refresh/ping state не получает от S40 будущую implementation card.
    let private_request_state = required_row_by_field(
        required_array(&profile, "request_material_fields"),
        "path",
        "_bunnycdn_ping_data|_cookie_refresh_params",
    );
    // Private API material исключает всю provider row.
    assert_eq!(
        required_string(private_request_state, "decision"),
        "private_api_target_row_excluded"
    );
    // Возврат возможен только через новую S00 public-serializable profile extension.
    assert_eq!(
        required_string(private_request_state, "future_session"),
        "none_without_public_serializable_profile_extension"
    );

    // Aggregate private live downloader exclusion остаётся authoritative.
    let private_live_exclusion = required_row_by_field(
        required_array(&profile, "excluded_rows"),
        "id",
        "private-live-downloaders",
    );
    // Это hard profile exclusion, а не Planned generic provider.
    assert_eq!(
        required_string(private_live_exclusion, "status"),
        "ProfileExcluded"
    );
    // Причина закрепляет ownership живых Python objects, threads и cookie state.
    assert_eq!(
        required_string(private_live_exclusion, "reason"),
        "requires_private_python_objects_threads_or_mutable_cookie_state"
    );
}

/// Synthetic fixture сохраняет классификацию, но не переносит live secrets/state.
#[test]
fn private_live_fixture_remains_secret_safe_and_non_reconstructible() {
    // Загружаем synthetic evidence, не выполняя provider или network I/O.
    let fixture_document = load_json_document(EXCLUSION_FIXTURE_PATH);
    // Находим exact fixture, указанный request-material decision.
    let private_live_fixture = required_row_by_field(
        required_array(&fixture_document, "fixtures"),
        "fixture_id",
        "excluded-private-live-state",
    );
    // Fixture обязан доказывать именно потерю live extractor semantics.
    assert_eq!(
        required_string(private_live_fixture, "expected_classification"),
        "RequiresLiveExtractorState"
    );
    // Payload остаётся synthetic JSON object.
    let payload = private_live_fixture
        .get("payload")
        .unwrap_or_else(|| panic!("private live fixture payload отсутствует"));
    // Exact protocol identity не превращается в generic websocket provider.
    assert_eq!(required_string(payload, "protocol"), "niconico_live");
    // Downloader options обязаны содержать только synthetic repr marker.
    let downloader_options = payload
        .get("downloader_options")
        .unwrap_or_else(|| panic!("synthetic downloader_options отсутствует"));
    // Repr marker явно не является replayable WebSocket handle.
    assert_eq!(
        required_string(downloader_options, "ws"),
        "<repr-websocket-response-not-reconstructible>"
    );
    // Fixture-level redaction policy запрещает реальные socket/cookie/provider captures.
    assert_eq!(
        required_string(&fixture_document, "redaction"),
        "no live object, socket, cookie, header, user locator, or provider capture is present"
    );
}
