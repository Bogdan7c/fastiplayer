//! Hermetic S41 gate для cross-provider runtime coverage и общего install path.

// Упорядоченные коллекции делают diagnostics воспроизводимыми между платформами.
use std::collections::{BTreeMap, BTreeSet};
// Checked-in evidence читается только из текущего workspace без network I/O.
use std::fs;
// Typed paths исключают зависимость теста от process working directory.
use std::path::PathBuf;

// JSON value позволяет проверять canonical manifests без production DTO и API.
use serde_json::Value;

// S00 profile остаётся owner-ом approved target inventory.
const PROFILE_PATH: &str = "compatibility/2026.07.04/profile.json";
// S41 manifest отдельно фиксирует runtime status и integration evidence.
const COVERAGE_PATH: &str = "compatibility/2026.07.04/runtime-coverage-s41.json";
// Exact число реально реализованных S00 rows на момент закрытия S41.
const IMPLEMENTED_ROW_COUNT: usize = 12;
// Aggregate RTMP identity не является доказанным wire provider-ом.
const RTMP_IDENTITY_ONLY_ROW: &str = "rtmp-family-flv";

/// Возвращает workspace root через compile-time path текущего crate-а.
fn workspace_root() -> PathBuf {
    // `service-ytdlp` находится ровно в `<workspace>/crates/service-ytdlp`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("service-ytdlp обязан находиться внутри workspace/crates")
        .to_path_buf()
}

/// Загружает обязательный checked-in JSON document.
fn load_json_document(crate_relative_path: &str) -> Value {
    // Profile artifacts принадлежат текущему crate и не зависят от cwd.
    let document_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(crate_relative_path);
    // Missing evidence является infrastructure failure, а не пустым coverage.
    let document_bytes = fs::read(&document_path).unwrap_or_else(|error| {
        panic!("не удалось прочитать {}: {error}", document_path.display())
    });
    // Невалидный JSON обязан немедленно уронить hermetic gate.
    serde_json::from_slice(&document_bytes)
        .unwrap_or_else(|error| panic!("не удалось разобрать {}: {error}", document_path.display()))
}

/// Возвращает обязательный JSON object.
fn required_object<'value>(
    value: &'value Value,
    field: &str,
) -> &'value serde_json::Map<String, Value> {
    // Missing или non-object поле означает schema regression.
    value
        .get(field)
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("обязательный object `{field}` отсутствует"))
}

/// Возвращает обязательный JSON array.
fn required_array<'value>(value: &'value Value, field: &str) -> &'value [Value] {
    // Missing или non-array поле не может превратиться в доказанный empty set.
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("обязательный array `{field}` отсутствует"))
}

/// Возвращает обязательное строковое поле.
fn required_string<'value>(value: &'value Value, field: &str) -> &'value str {
    // Missing или non-string поле делает row неоднозначной.
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("обязательное строковое поле `{field}` отсутствует"))
}

/// Преобразует строковый JSON array в exact ordered vector.
fn required_string_array<'value>(value: &'value Value, field: &str) -> Vec<&'value str> {
    // Каждый элемент обязан быть string identity без implicit coercion.
    required_array(value, field)
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .unwrap_or_else(|| panic!("`{field}` обязан содержать только строки"))
        })
        .collect()
}

/// Индексирует rows по stable `id` и отвергает duplicates.
fn rows_by_id<'value>(rows: &'value [Value], owner: &str) -> BTreeMap<&'value str, &'value Value> {
    // Ordered map сохраняет стабильный порядок failure diagnostics.
    let mut indexed_rows = BTreeMap::new();
    // Каждая row проходит exact identity admission.
    for row in rows {
        // Stable ID берётся без normalization или alias expansion.
        let row_id = required_string(row, "id");
        // Duplicate identity запрещает доказать one-to-one coverage.
        assert!(
            indexed_rows.insert(row_id, row).is_none(),
            "{owner} содержит duplicate row `{row_id}`"
        );
    }
    // Готовый map используется только read-only assertions.
    indexed_rows
}

/// Проверяет, что declared source evidence существует и содержит exact symbol.
fn assert_source_evidence(evidence: &Value, context: &str) {
    // Путь хранится workspace-relative и не может зависеть от local checkout path.
    let evidence_path = required_string(evidence, "path");
    // Symbol является стабильным именем focused test или production boundary.
    let evidence_symbol = required_string(evidence, "symbol");
    // Exact workspace path строится без glob или directory traversal из runtime input.
    let absolute_path = workspace_root().join(evidence_path);
    // Missing file означает stale либо выдуманное traceability evidence.
    let source = fs::read_to_string(&absolute_path).unwrap_or_else(|error| {
        panic!(
            "{context}: не удалось прочитать evidence {}: {error}",
            absolute_path.display()
        )
    });
    // Exact symbol обязан оставаться в declared owner file.
    assert!(
        source.contains(evidence_symbol),
        "{context}: symbol `{evidence_symbol}` отсутствует в `{evidence_path}`"
    );
}

/// S41 runtime rows один-к-одному закрывают canonical S00 target inventory.
#[test]
fn runtime_coverage_resolves_every_s00_target_without_planned_or_guessed_provider() {
    // Загружаем immutable S00 inventory и отдельный runtime-status owner S41.
    let profile = load_json_document(PROFILE_PATH);
    // Coverage document не модифицирует значения S00 `Target`.
    let coverage = load_json_document(COVERAGE_PATH);
    // Versioned schema не позволяет silently reinterpret старый artifact.
    assert_eq!(
        coverage.get("schema_version").and_then(Value::as_u64),
        Some(1)
    );
    // Runtime coverage обязано относиться к exact canonical profile.
    assert_eq!(
        required_string(&coverage, "profile_id"),
        required_string(&profile, "profile_id")
    );
    // Session identity отделяет этот gate от будущего S42 acceptance.
    assert_eq!(required_string(&coverage, "session"), "S41");

    // S00 rows являются authoritative approved target set.
    let profile_rows = rows_by_id(required_array(&profile, "target_rows"), "S00 profile");
    // S41 rows являются authoritative runtime disposition set.
    let coverage_rows = rows_by_id(required_array(&coverage, "rows"), "S41 coverage");
    // Ни одна target row не может исчезнуть или появиться только в runtime manifest.
    assert_eq!(
        profile_rows.keys().copied().collect::<BTreeSet<_>>(),
        coverage_rows.keys().copied().collect::<BTreeSet<_>>()
    );

    // Счётчик доказывает точный Implemented set без RTMP/special guessing.
    let mut implemented_count = 0_usize;
    // Каждая row сверяется с canonical owner sessions и checked-in evidence.
    for (row_id, coverage_row) in coverage_rows {
        // Matching canonical row уже доказана exact set comparison выше.
        let profile_row = profile_rows
            .get(row_id)
            .expect("matching S00 row обязана существовать");
        // S41 не переписывает dependency ownership предыдущих sessions.
        assert_eq!(
            required_string_array(coverage_row, "owner_sessions"),
            required_string_array(profile_row, "future_sessions"),
            "owner sessions расходятся для `{row_id}`"
        );
        // Provider family обязана быть явной и непустой.
        assert!(
            !required_string(coverage_row, "provider_family").is_empty(),
            "provider family пуста для `{row_id}`"
        );
        // Runtime status не допускает незавершённый Planned внутри закрытого S41.
        match required_string(coverage_row, "status") {
            // Implemented row участвует в playback integration matrix.
            "Implemented" => {
                // Aggregate RTMP row не может получить fake provider admission.
                assert_ne!(row_id, RTMP_IDENTITY_ONLY_ROW);
                // Exact реализованный inventory считается один раз.
                implemented_count += 1;
            }
            // Единственная unresolved wire family остаётся explicit profile exclusion.
            "ProfileExcluded" => {
                // Никакую другую approved exact row S41 молча исключать не должен.
                assert_eq!(row_id, RTMP_IDENTITY_ONLY_ROW);
            }
            // Любой новый status требует отдельного schema/architecture решения.
            unexpected_status => {
                panic!("недопустимый S41 runtime status `{unexpected_status}` для `{row_id}`")
            }
        }
        // Каждая disposition row обязана иметь хотя бы одно focused evidence.
        let evidence_rows = required_array(coverage_row, "evidence");
        // Empty array не доказывает provider либо exclusion.
        assert!(!evidence_rows.is_empty(), "нет evidence для `{row_id}`");
        // Все declared tests проверяются как реальные workspace symbols.
        for evidence in evidence_rows {
            // Context делает failure actionable без раскрытия media locator-а.
            assert_source_evidence(evidence, row_id);
        }
    }
    // Exact count защищает список двенадцати production-implemented rows.
    assert_eq!(implemented_count, IMPLEMENTED_ROW_COUNT);
}

/// Все Implemented rows сходятся к одним app/player boundaries и общим regressions.
#[test]
fn implemented_rows_share_candidate_to_strong_install_path_and_cross_cutting_evidence() {
    // Загружаем только hermetic S41 artifact.
    let coverage = load_json_document(COVERAGE_PATH);
    // Общий path является object-ом named stages, а не positional array.
    let common_path = required_object(&coverage, "common_integration_path");
    // Exact stage set не позволяет случайно обойти PreparedMedia либо barrier.
    assert_eq!(
        common_path
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "candidate",
            "transport_and_demux",
            "prepared_media",
            "strong_install",
        ])
    );
    // Каждый stage обязан ссылаться на существующий production symbol.
    for (stage, evidence) in common_path {
        // Stage name не содержит locator или provider secret material.
        assert_source_evidence(evidence, stage);
    }

    // Cross-cutting requirements не дублируются provider-specific implementation-ом.
    let cross_cutting_evidence = required_array(&coverage, "cross_cutting_evidence");
    // Exact S41 checklist содержит двенадцать independent concerns.
    assert_eq!(cross_cutting_evidence.len(), 12);
    // Duplicate requirement мог бы скрыть отсутствующую acceptance axis.
    let mut requirements = BTreeSet::new();
    // Каждый requirement ссылается на уже существующий focused owner test.
    for evidence in cross_cutting_evidence {
        // Human-readable requirement является stable traceability key.
        let requirement = required_string(evidence, "requirement");
        // Duplicate keys запрещены.
        assert!(
            requirements.insert(requirement),
            "duplicate cross-cutting requirement `{requirement}`"
        );
        // Source symbol подтверждается тем же helper-ом.
        assert_source_evidence(evidence, requirement);
    }
}

/// Conditional provider cards остаются typed no-op/exclusion и не получают playback test.
#[test]
fn rtmp_live_supplements_and_special_providers_remain_explicit_non_playback_scope() {
    // S00 нужен для exact absence/exclusion assertions.
    let profile = load_json_document(PROFILE_PATH);
    // S41 хранит owner handoff conditional cards отдельно от runtime rows.
    let coverage = load_json_document(COVERAGE_PATH);
    // Conditional inventory должен явно перечислять S36/S38/S40.
    let conditional_rows = required_array(&coverage, "conditional_expansions");
    // Три conditional owner-а не могут исчезнуть из S41 handoff.
    assert_eq!(conditional_rows.len(), 3);
    // Owner identities проверяются exact и без alias normalization.
    let owners = conditional_rows
        .iter()
        .map(|row| required_string(row, "owner_session"))
        .collect::<BTreeSet<_>>();
    // S36 live, S38 live и S40 special остаются единственными conditional gaps.
    assert_eq!(owners, BTreeSet::from(["S36", "S38", "S40"]));
    // Ни один conditional owner не объявляется Implemented.
    for row in conditional_rows {
        // Допустимы только explicit exclusion либо доказанное отсутствие approved row.
        assert!(
            matches!(
                required_string(row, "status"),
                "ProfileExcluded" | "NoApprovedRow"
            ),
            "conditional owner не может быть playback-enabled"
        );
        // Evidence artifact обязан существовать даже для no-op.
        let evidence_path = required_string(row, "evidence_path");
        // Exact workspace-relative path проверяется без исполнения provider-а.
        assert!(
            workspace_root().join(evidence_path).is_file(),
            "conditional evidence `{evidence_path}` отсутствует"
        );
    }

    // S40 не может тайно появиться как target owner без новой profile extension.
    for target_row in required_array(&profile, "target_rows") {
        // Future sessions обязаны оставаться свободными от generic S40/S40P admission.
        for owner_session in required_string_array(target_row, "future_sessions") {
            // Exact S40 и generated S40P cards запрещены текущим no-op evidence.
            assert_ne!(owner_session, "S40");
            // Prefix check ловит будущую card без соответствующего S41 update.
            assert!(!owner_session.starts_with("S40P-"));
        }
    }
}
