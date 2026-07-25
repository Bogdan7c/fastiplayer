//! Общие строгие validators для S42 profile-traceability integration test.

// Ordered collections делают failure diagnostics детерминированными.
use std::collections::{BTreeMap, BTreeSet};
// Filesystem используется только для checked-in artifacts и source evidence.
use std::fs;
// PathBuf строит cwd-independent paths к checked-in artifacts.
use std::path::PathBuf;

// JSON schema проверяется без permissive defaults.
use serde_json::{Map, Value};

// Source/package/test-symbol validation имеет отдельный coherent owner.
#[path = "support/evidence_source.rs"]
mod evidence_source;

// Parent validator использует только узкие source-validation boundaries.
use evidence_source::{
    assert_real_production_symbol, assert_real_test_function, assert_test_package_and_target,
    source_with_exact_symbol,
};

/// Возвращает workspace root без зависимости от текущего рабочего каталога.
pub(super) fn workspace_root() -> PathBuf {
    // Cargo гарантирует absolute manifest directory текущего crate.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        // Первый parent поднимает путь из crate в `crates`.
        .parent()
        // Отсутствие parent означает сломанный repository layout.
        .expect("service-ytdlp должен находиться внутри workspace crates")
        // Второй parent поднимает путь в workspace root.
        .parent()
        // Этот parent также обязателен.
        .expect("workspace root должен быть parent каталога crates")
        // Owned path не удерживает borrow временного значения.
        .to_path_buf()
}

/// Загружает обязательный checked-in JSON document текущего crate.
pub(super) fn load_json_document(crate_relative_path: &str) -> Value {
    // Artifact path не зависит от cwd test runner-а.
    let document_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(crate_relative_path);
    // Missing artifact является infrastructure failure.
    let document_bytes = fs::read(&document_path).unwrap_or_else(|error| {
        panic!("не удалось прочитать {}: {error}", document_path.display())
    });
    // Невалидный JSON немедленно останавливает gate.
    serde_json::from_slice(&document_bytes)
        .unwrap_or_else(|error| panic!("не удалось разобрать {}: {error}", document_path.display()))
}

/// Возвращает обязательный JSON object field.
pub(super) fn required_object<'value>(
    value: &'value Value,
    field: &str,
) -> &'value Map<String, Value> {
    // Object не заменяется permissive default-ом.
    value
        .get(field)
        // Exact JSON type обязателен.
        .and_then(Value::as_object)
        // Failure называет точное поле.
        .unwrap_or_else(|| panic!("обязательный object `{field}` отсутствует"))
}

/// Возвращает обязательный JSON array field.
pub(super) fn required_array<'value>(value: &'value Value, field: &str) -> &'value [Value] {
    // Array не заменяется пустым fallback-ом.
    value
        .get(field)
        // Exact JSON type обязателен.
        .and_then(Value::as_array)
        // Borrowed slice удобен для read-only gate.
        .map(Vec::as_slice)
        // Missing array является schema failure.
        .unwrap_or_else(|| panic!("обязательный array `{field}` отсутствует"))
}

/// Возвращает обязательный JSON string field.
pub(super) fn required_string<'value>(value: &'value Value, field: &str) -> &'value str {
    // Exact string не нормализуется.
    value
        .get(field)
        // Неверный тип не игнорируется.
        .and_then(Value::as_str)
        // Diagnostics называют concrete field.
        .unwrap_or_else(|| panic!("обязательная строка `{field}` отсутствует"))
}

/// Возвращает обязательный nullable string field.
pub(super) fn required_nullable_string<'value>(
    value: &'value Value,
    field: &str,
) -> Option<&'value str> {
    // Explicit null отличается от missing field.
    match value
        .get(field)
        // Missing nullable field всё равно является schema drift.
        .unwrap_or_else(|| panic!("обязательное nullable поле `{field}` отсутствует"))
    {
        // Null фиксирует доказанное отсутствие identity.
        Value::Null => None,
        // String сохраняется exact.
        Value::String(text) => Some(text),
        // Другие JSON types запрещены.
        _ => panic!("nullable поле `{field}` обязано быть строкой либо null"),
    }
}

/// Возвращает обязательный JSON boolean field.
pub(super) fn required_bool(value: &Value, field: &str) -> bool {
    // Boolean нельзя заменить truthy scalar-ом.
    value
        .get(field)
        // Exact type обязателен.
        .and_then(Value::as_bool)
        // Missing flag является schema failure.
        .unwrap_or_else(|| panic!("обязательный bool `{field}` отсутствует"))
}

/// Возвращает обязательное non-negative JSON integer field.
pub(super) fn required_usize(value: &Value, field: &str) -> usize {
    // JSON number сначала проверяется как u64.
    let raw_value = value
        .get(field)
        // Negative/floating-point значения не допускаются.
        .and_then(Value::as_u64)
        // Missing number является schema failure.
        .unwrap_or_else(|| panic!("обязательное целое `{field}` отсутствует"));
    // Conversion остаётся checked на 32-bit targets.
    usize::try_from(raw_value)
        // Oversized count не может описывать repository inventory.
        .unwrap_or_else(|_| panic!("`{field}` не помещается в usize"))
}

/// Возвращает обязательный string array без silent filtering.
pub(super) fn required_string_array<'value>(value: &'value Value, field: &str) -> Vec<&'value str> {
    // Каждый element обязан быть exact string.
    required_array(value, field)
        .iter()
        // Неверный element type не пропускается.
        .map(|entry| {
            entry
                .as_str()
                // Failure указывает owner field.
                .unwrap_or_else(|| panic!("`{field}` содержит нестроковый element"))
        })
        // Owned vector удобен для ordered и set assertions.
        .collect()
}

/// Возвращает обязательный unique string set без потери duplicate diagnostics.
pub(super) fn required_unique_string_set<'value>(
    value: &'value Value,
    field: &str,
    context: &str,
) -> BTreeSet<&'value str> {
    // Исходный array проверяется без silent filtering.
    let string_values = required_string_array(value, field);
    // Ordered set используется для exact inventory comparisons.
    let unique_values = string_values.iter().copied().collect::<BTreeSet<_>>();
    // Duplicate identity не должна скрываться set-преобразованием.
    assert_eq!(
        unique_values.len(),
        string_values.len(),
        "{context} содержит duplicate `{field}` identity"
    );
    // Borrowed set сохраняет связь с manifest document.
    unique_values
}

/// Индексирует rows по stable `id` и отвергает duplicates.
pub(super) fn rows_by_id<'value>(
    rows: &'value [Value],
    owner: &str,
) -> BTreeMap<&'value str, &'value Value> {
    // Ordered map сохраняет стабильный failure order.
    let mut indexed_rows = BTreeMap::new();
    // Каждая row проходит exact identity admission.
    for row in rows {
        // Stable ID берётся без alias normalization.
        let row_id = required_string(row, "id");
        // Duplicate identity разрушает one-to-one traceability.
        assert!(
            indexed_rows.insert(row_id, row).is_none(),
            "{owner} содержит duplicate row `{row_id}`"
        );
    }
    // Map остаётся read-only у вызывающего gate.
    indexed_rows
}

/// Возвращает единственную row с exact string field identity.
pub(super) fn required_row_by_string_field<'value>(
    rows: &'value [Value],
    field: &str,
    expected_identity: &str,
    owner: &str,
) -> &'value Value {
    // Все matching rows собираются, чтобы duplicate identity не прошла молча.
    let matching_rows = rows
        .iter()
        // Exact string field обязателен у каждой candidate row.
        .filter(|row| required_string(row, field) == expected_identity)
        // Borrowed vector позволяет отдельно проверить cardinality.
        .collect::<Vec<_>>();
    // Ровно одна row должна владеть requested identity.
    assert_eq!(
        matching_rows.len(),
        1,
        "{owner} должен содержать ровно одну row `{field}={expected_identity}`"
    );
    // Cardinality assertion гарантирует безопасный первый element.
    matching_rows[0]
}

/// Проверяет unique evidence references одного role/decision.
pub(super) fn required_evidence_ids<'value>(
    value: &'value Value,
    field: &str,
    context: &str,
) -> Vec<&'value str> {
    // Exact field различает runtime evidence и owner approval.
    let evidence_ids = required_string_array(value, field);
    // Set нужен только для duplicate detection.
    let unique_ids = evidence_ids.iter().copied().collect::<BTreeSet<_>>();
    // Duplicate не должен искусственно увеличивать coverage.
    assert_eq!(
        unique_ids.len(),
        evidence_ids.len(),
        "{context} содержит duplicate `{field}` reference"
    );
    // Exact source order сохраняется.
    evidence_ids
}

/// Рекурсивно собирает все fields, заканчивающиеся на `evidence_ids`.
pub(super) fn collect_referenced_evidence_ids(
    value: &Value,
    referenced_ids: &mut BTreeSet<String>,
) {
    // Object может содержать row, decision или nested exception.
    if let Some(object) = value.as_object() {
        // Обходим fields без предположения об их JSON order.
        for (field, nested_value) in object {
            // Runtime и approval evidence используют общий typed suffix.
            if field.ends_with("evidence_ids") {
                // Reference list обязан быть array.
                let evidence_values = nested_value
                    .as_array()
                    // Неверный type не маскируется пустым list.
                    .unwrap_or_else(|| panic!("`{field}` обязан быть array"));
                // Каждый reference проверяется отдельно.
                for evidence_value in evidence_values {
                    // Exact ID обязан быть string.
                    let evidence_id = evidence_value
                        .as_str()
                        // Неверный element type является schema failure.
                        .unwrap_or_else(|| panic!("`{field}` содержит нестроковый element"));
                    // Один catalog item может обслуживать несколько rows.
                    referenced_ids.insert(evidence_id.to_owned());
                }
            } else {
                // Остальные fields проверяются рекурсивно.
                collect_referenced_evidence_ids(nested_value, referenced_ids);
            }
        }
    } else if let Some(array) = value.as_array() {
        // Arrays могут содержать rows или decisions.
        for nested_value in array {
            // Каждый element проходит тот же traversal.
            collect_referenced_evidence_ids(nested_value, referenced_ids);
        }
    }
}

/// Рекурсивно запрещает unresolved `Planned` status.
pub(super) fn assert_no_planned_status(value: &Value, context: &str) {
    // Object fields проверяются по exact key.
    if let Some(object) = value.as_object() {
        // Каждый nested status участвует в gate.
        for (field, nested_value) in object {
            // Exact `status` является disposition.
            if field == "status" {
                // Status обязан быть string.
                let status = nested_value
                    .as_str()
                    // Неверный type не может пройти profile-traceability gate.
                    .unwrap_or_else(|| panic!("{context}: status обязан быть строкой"));
                // S42 не допускает unresolved roadmap state.
                assert_ne!(status, "Planned", "{context} содержит Planned status");
            } else {
                // Остальные fields проверяются рекурсивно.
                assert_no_planned_status(nested_value, context);
            }
        }
    } else if let Some(array) = value.as_array() {
        // Arrays могут содержать nested disposition rows.
        for nested_value in array {
            // Каждый element проверяется тем же invariant.
            assert_no_planned_status(nested_value, context);
        }
    }
}

/// Проверяет kind-specific shape одного catalog entry.
pub(super) fn assert_typed_evidence(evidence_id: &str, evidence: &Value) {
    // Common path/symbol validation выполняется до kind-specific assertions.
    let source = source_with_exact_symbol(evidence_id, evidence);
    // Role обязана быть explicit non-empty string.
    let evidence_role = required_string(evidence, "role");
    // Empty role не может участвовать в typed traceability.
    assert!(
        !evidence_role.is_empty(),
        "evidence `{evidence_id}` содержит пустую role"
    );
    // Kind discriminator не допускает structural guessing.
    match required_string(evidence, "kind") {
        // Production boundary физически отделён от test evidence.
        "ProductionBoundary" => {
            // Production boundary допустим только для реально production-owned ролей.
            assert!(matches!(
                evidence_role,
                "provider" | "demux" | "exclusion" | "hardware_exception"
            ));
            // Production entry не должен притворяться Cargo test-ом.
            assert_eq!(
                evidence
                    .as_object()
                    .expect("typed evidence обязан быть object")
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["kind", "role", "path", "symbol"])
            );
            // Declaration prefix обязан быть реальным production token, а не comment/string.
            assert_real_production_symbol(evidence_id, evidence, &source);
        }
        // Executable test требует package/target/polarity и реальный attribute.
        "ExecutableTest" => {
            // Executable test не может подменять checked-in owner approval.
            assert!(matches!(
                evidence_role,
                "provider"
                    | "demux"
                    | "decoder"
                    | "runtime_fixture"
                    | "capability"
                    | "exclusion"
                    | "quality"
                    | "cross_cutting"
                    | "hardware_exception"
            ));
            // Test shape имеет exact versioned fields.
            assert_eq!(
                evidence
                    .as_object()
                    .expect("typed evidence обязан быть object")
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    "kind", "role", "polarity", "package", "target", "path", "symbol",
                ])
            );
            // Positive/Negative distinction запрещает считать rejection единственным success proof.
            assert!(
                matches!(
                    required_string(evidence, "polarity"),
                    "Positive" | "Negative"
                ),
                "test evidence `{evidence_id}` имеет unknown polarity"
            );
            // Package и Cargo target должны существовать реально.
            assert_test_package_and_target(evidence_id, evidence);
            // Symbol обязан быть executable standard test.
            assert_real_test_function(evidence_id, evidence, &source);
        }
        // Owner approval хранится checked-in, но не выдаётся за executable test.
        "CheckedInDecision" => {
            // Decision entry имеет только common typed fields.
            assert_eq!(
                evidence
                    .as_object()
                    .expect("typed evidence обязан быть object")
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["kind", "role", "path", "symbol"])
            );
            // Только owner-approval role может использовать этот kind.
            assert_eq!(evidence_role, "owner_approval");
        }
        // Любой новый kind требует schema bump.
        unexpected_kind => {
            panic!("evidence `{evidence_id}` содержит unknown kind `{unexpected_kind}`")
        }
    }
}

/// Проверяет существование references и их compatibility с expected role.
pub(super) fn assert_evidence_role(
    evidence_catalog: &Map<String, Value>,
    evidence_ids: &[&str],
    expected_role: &str,
    context: &str,
) {
    // Пустой reference list не может доказать role либо exclusion.
    assert!(
        !evidence_ids.is_empty(),
        "{context} содержит пустое evidence"
    );
    // Каждый reference проверяется independently.
    for evidence_id in evidence_ids {
        // Reference обязан разрешаться в catalog.
        let evidence = evidence_catalog
            .get(*evidence_id)
            // Missing catalog entry является stale manifest.
            .unwrap_or_else(|| panic!("{context} ссылается на unknown evidence `{evidence_id}`"));
        // Catalog role должна совпасть с claim owner-ом.
        assert_eq!(
            required_string(evidence, "role"),
            expected_role,
            "{context}: evidence `{evidence_id}` имеет incompatible role"
        );
    }
}

/// Возвращает polarity set только executable evidence references.
pub(super) fn executable_polarities<'catalog>(
    evidence_catalog: &'catalog Map<String, Value>,
    evidence_ids: &[&str],
) -> BTreeSet<&'catalog str> {
    // Set делает positive/negative assertion независимым от source order.
    evidence_ids
        .iter()
        // Catalog resolution уже проверяется caller-ом.
        .filter_map(|evidence_id| evidence_catalog.get(*evidence_id))
        // Production/decision entries не имеют polarity.
        .filter(|evidence| required_string(evidence, "kind") == "ExecutableTest")
        // Exact polarity string возвращается borrowed.
        .map(|evidence| required_string(evidence, "polarity"))
        // Duplicate polarity не влияет на semantic set.
        .collect()
}

/// Возвращает exact VA-API arm inventory до owner-approved Baseline addition.
pub(super) fn expected_vaapi_baseline_profile_arms() -> BTreeSet<&'static str> {
    // Historical baseline фиксируется в коде gate, а не доверяется изменяемому manifest.
    BTreeSet::from([
        // VP9 Profile 0 уже был частью hardware capability.
        "VAProfileVP9Profile0",
        // VP9 Profile 1 уже был частью hardware capability.
        "VAProfileVP9Profile1",
        // VP9 Profile 2 уже был частью hardware capability.
        "VAProfileVP9Profile2",
        // VP9 Profile 3 уже был частью hardware capability.
        "VAProfileVP9Profile3",
        // AV1 Main 8/10-bit slot существовал до exception.
        "VAProfileAV1Profile0",
        // AV1 high-bit-depth slot существовал до exception.
        "VAProfileAV1Profile1",
        // Constrained Baseline не является ordinary Baseline alias.
        "VAProfileH264ConstrainedBaseline",
        // H.264 Main уже поддерживался.
        "VAProfileH264Main",
        // H.264 High уже поддерживался.
        "VAProfileH264High",
        // HEVC Main уже поддерживался.
        "VAProfileHEVCMain",
        // HEVC Main10 уже поддерживался.
        "VAProfileHEVCMain10",
        // HEVC Main12 уже поддерживался.
        "VAProfileHEVCMain12",
        // HEVC 4:2:2 10-bit уже поддерживался.
        "VAProfileHEVCMain422_10",
        // HEVC 4:2:2 12-bit уже поддерживался.
        "VAProfileHEVCMain422_12",
        // HEVC 4:4:4 8-bit уже поддерживался.
        "VAProfileHEVCMain444",
        // HEVC 4:4:4 10-bit уже поддерживался.
        "VAProfileHEVCMain444_10",
        // HEVC 4:4:4 12-bit уже поддерживался.
        "VAProfileHEVCMain444_12",
        // HEVC SCC Main уже поддерживался.
        "VAProfileHEVCSccMain",
        // HEVC SCC Main10 уже поддерживался.
        "VAProfileHEVCSccMain10",
        // HEVC SCC Main444 уже поддерживался.
        "VAProfileHEVCSccMain444",
        // HEVC SCC Main444 10-bit уже поддерживался.
        "VAProfileHEVCSccMain444_10",
        // VP8 hardware slot уже поддерживался.
        "VAProfileVP8Version0_3",
    ])
}

/// Связывает versioned VA-API inventories с exact production match arms.
pub(super) fn assert_vaapi_profile_arm_sets(hardware_exception: &Value) {
    // Versioned arm sets отделяют historical baseline от current source.
    let arm_sets = hardware_exception
        .get("vaapi_profile_arm_sets")
        // Missing sets не доказывают unchanged hardware inventory.
        .unwrap_or_else(|| panic!("VA-API profile arm sets отсутствуют"));
    // Arm-set object содержит только baseline/current и approved delta fields.
    assert_eq!(
        arm_sets
            .as_object()
            // Scalar не может описывать versioned inventories.
            .expect("VA-API profile arm sets обязаны быть object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "baseline",
            "current",
            "approved_added_delta",
            "approved_removed_delta",
        ])
    );
    // Historical inventory locked в test code, а не доверяется manifest-у.
    let baseline_arms =
        required_unique_string_set(arm_sets, "baseline", "VA-API historical baseline");
    // Baseline не может расшириться вместе с current source незаметно.
    assert_eq!(baseline_arms, expected_vaapi_baseline_profile_arms());
    // Current manifest inventory также запрещает duplicate arms.
    let current_arms = required_unique_string_set(arm_sets, "current", "VA-API current arms");
    // Production extractor читает только exact formats_for_va_profile match.
    let source_current_arms = current_vaapi_profile_match_arms();
    // Borrowed view позволяет сравнить source identities без normalization.
    let source_current_arm_refs = source_current_arms
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    // Manifest current inventory обязан точно совпасть с production source.
    assert_eq!(current_arms, source_current_arm_refs);
    // Approved added delta также является unique exact set.
    let approved_added_arms =
        required_unique_string_set(arm_sets, "approved_added_delta", "VA-API added delta");
    // Единственное разрешённое расширение — ordinary H.264 Baseline.
    assert_eq!(
        approved_added_arms,
        BTreeSet::from(["VAProfileH264Baseline"])
    );
    // Фактическая разница current minus baseline обязана совпасть с approval.
    assert_eq!(
        current_arms
            .difference(&baseline_arms)
            .copied()
            .collect::<BTreeSet<_>>(),
        approved_added_arms
    );
    // Removed delta хранится explicit и обязан оставаться пустым.
    let approved_removed_arms =
        required_unique_string_set(arm_sets, "approved_removed_delta", "VA-API removed delta");
    // Owner не одобрял удаление старой hardware capability.
    assert!(approved_removed_arms.is_empty());
    // Production current не может фактически потерять baseline arm.
    assert_eq!(
        baseline_arms
            .difference(&current_arms)
            .copied()
            .collect::<BTreeSet<_>>(),
        approved_removed_arms
    );
}

/// Извлекает exact `formats_for_va_profile` match-arm identities.
pub(super) fn current_vaapi_profile_match_arms() -> BTreeSet<String> {
    // Production source boundary зафиксирован hardware exception.
    let probe_path = "crates/video-vaapi/src/probe.rs";
    // Source читается через workspace-owned path.
    let source = fs::read_to_string(workspace_root().join(probe_path))
        // Missing production file является hard failure.
        .unwrap_or_else(|error| panic!("не удалось прочитать `{probe_path}`: {error}"));
    // Начало exact function отделяет production arms от tests.
    let function_start = source
        .find("fn formats_for_va_profile(")
        // Missing function означает boundary rename без manifest update.
        .unwrap_or_else(|| panic!("`formats_for_va_profile` boundary отсутствует"));
    // Retain line находится после match и завершает arm inventory.
    let function_tail = &source[function_start..];
    // End marker исключает profile mentions из comments/tests ниже.
    let match_end = function_tail
        .find("formats.retain(")
        // Missing retain меняет capability semantics.
        .unwrap_or_else(|| panic!("`formats_for_va_profile` retain boundary отсутствует"));
    // Только match section участвует в extraction.
    let match_source = &function_tail[..match_end];
    // Ordered set защищает exact arm identity без source-order coupling.
    let mut profile_arms = BTreeSet::new();
    // Каждый source line может содержать не более одной arm identity.
    for source_line in match_source.lines() {
        // Prefix однозначно отделяет libva enum variant.
        let Some(profile_suffix) = source_line.split("libva::VAProfile::").nth(1) else {
            // Lines без profile identity не участвуют.
            continue;
        };
        // Только match arms, а не type mentions, содержат arrow.
        if !profile_suffix.contains("=>") {
            // Function parameter type не является arm.
            continue;
        }
        // Variant name заканчивается перед whitespace либо punctuation.
        let profile_name = profile_suffix
            .chars()
            // Rust enum variants состоят из alphanumeric и underscore.
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            // String нужен для owned set.
            .collect::<String>();
        // Empty extraction означает malformed source pattern.
        assert!(!profile_name.is_empty());
        // Duplicate arm identity не допускается Rust match-ом и manifest gate-ом.
        assert!(
            profile_arms.insert(profile_name.clone()),
            "duplicate VA-API profile arm `{profile_name}`"
        );
    }
    // Empty set означал бы, что parser больше не видит production match.
    assert!(!profile_arms.is_empty());
    // Exact current arm set возвращается hardware gate.
    profile_arms
}
