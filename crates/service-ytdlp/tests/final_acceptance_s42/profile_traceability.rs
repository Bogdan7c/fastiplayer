//! Exact S42 schema и one-to-one traceability утверждённых profile rows.

// Ordered collections дают стабильные diagnostics для exact identity assertions.
use std::collections::{BTreeMap, BTreeSet};

// JSON values остаются immutable checked-in acceptance artifacts.
use serde_json::{Map, Value};

// Root владеет canonical artifact paths и exact profile vocabulary.
use super::{
    APPROVED_ROW_COUNT, IMPLEMENTED_ROW_COUNT, PROFILE_PATH, REQUIRED_ROLES,
    RTMP_IDENTITY_ONLY_ROW, S41_COVERAGE_PATH, S42_ACCEPTANCE_PATH,
};
// Focused module зависит только от общего JSON/evidence facade.
use super::support::{
    assert_evidence_role, assert_no_planned_status, executable_polarities, load_json_document,
    required_array, required_evidence_ids, required_object, required_string, required_string_array,
    rows_by_id,
};

/// Immutable документы, участвующие в one-to-one profile traceability.
struct TraceabilityDocuments {
    /// Canonical S00 compatibility profile.
    profile: Value,
    /// Утверждённый S41 runtime disposition.
    s41_coverage: Value,
    /// Scoped S42 traceability decision.
    s42_acceptance: Value,
}

impl TraceabilityDocuments {
    /// Загружает exact S00/S41/S42 artifacts без скрытого fallback.
    fn load() -> Self {
        // S00 задаёт approved identity set.
        let profile = load_json_document(PROFILE_PATH);
        // S41 задаёт предыдущий runtime disposition.
        let s41_coverage = load_json_document(S41_COVERAGE_PATH);
        // S42 задаёт только scoped profile traceability.
        let s42_acceptance = load_json_document(S42_ACCEPTANCE_PATH);
        // Все документы остаются read-only внутри focused tests.
        Self {
            profile,
            s41_coverage,
            s42_acceptance,
        }
    }

    /// Индексирует canonical S00 target rows.
    fn profile_rows(&self) -> BTreeMap<&str, &Value> {
        // Stable IDs остаются единственным ключом.
        rows_by_id(required_array(&self.profile, "target_rows"), "S00 profile")
    }

    /// Индексирует canonical S00 excluded rows.
    fn profile_excluded_rows(&self) -> BTreeMap<&str, &Value> {
        // Exact exclusion identities не alias-нормализуются.
        rows_by_id(
            required_array(&self.profile, "excluded_rows"),
            "S00 exclusions",
        )
    }

    /// Индексирует immutable S41 handoff rows.
    fn s41_rows(&self) -> BTreeMap<&str, &Value> {
        // S41 inventory сравнивается по stable row ID.
        rows_by_id(required_array(&self.s41_coverage, "rows"), "S41 coverage")
    }

    /// Индексирует scoped S42 acceptance rows.
    fn s42_rows(&self) -> BTreeMap<&str, &Value> {
        // S42 обязан быть exact projection canonical inventory.
        rows_by_id(
            required_array(&self.s42_acceptance, "rows"),
            "S42 acceptance",
        )
    }
}

/// Возвращает roles одной row и закрепляет exact typed schema.
fn exact_row_roles<'row>(row_id: &str, row: &'row Value) -> &'row Map<String, Value> {
    // Role value обязан быть JSON object.
    let roles = required_object(row, "roles");
    // Extra либо missing role требуют нового schema decision.
    assert_eq!(
        roles.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        REQUIRED_ROLES.into_iter().collect::<BTreeSet<_>>(),
        "typed role set расходится для `{row_id}`"
    );
    // Проверенный object возвращается focused role tests.
    roles
}

/// S42 artifact имеет exact scoped schema и не содержит unresolved Planned state.
#[test]
fn scoped_traceability_decision_schema_is_exact() {
    // Загружаем только документы, определяющие exact profile identity и S42 scope.
    let documents = TraceabilityDocuments::load();
    // Versioned schema предотвращает silent reinterpretation.
    assert_eq!(
        documents
            .s42_acceptance
            .get("schema_version")
            .and_then(Value::as_u64),
        Some(1)
    );
    // Artifact относится к exact canonical profile.
    assert_eq!(
        required_string(&documents.s42_acceptance, "profile_id"),
        required_string(&documents.profile, "profile_id")
    );
    // Session identity отличает final gate от S41.
    assert_eq!(required_string(&documents.s42_acceptance, "session"), "S42");
    // Decision value нужен одновременно для object shape и exact fields.
    let decision_value = documents
        .s42_acceptance
        .get("decision")
        // Missing decision делает artifact невалидным.
        .expect("S42 decision value обязано существовать");
    // Top-level decision относится только к profile traceability.
    let decision = required_object(&documents.s42_acceptance, "decision");
    // Scope не выдаёт этот artifact за полную final acceptance.
    assert_eq!(
        required_string(decision_value, "scope"),
        "ProfileTraceability"
    );
    // Checked-in runtime fixtures делают scoped traceability complete.
    assert_eq!(
        required_string(decision_value, "status"),
        "ProfileTraceabilityComplete"
    );
    // Complete scoped decision не допускает скрытого unresolved evidence gap.
    assert!(
        required_array(&documents.s42_acceptance, "traceability_gaps").is_empty(),
        "ProfileTraceabilityComplete не может содержать traceability gap"
    );
    // Artifact явно не принимает manual/full release scopes.
    assert_eq!(
        required_string_array(decision_value, "does_not_accept")
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "full final acceptance",
            "manual opt-in acceptance",
            "hardware manual rerun",
        ])
    );
    // Decision object не допускает неявные status fields.
    assert_eq!(decision.len(), 3);
    // Ни один nested profile disposition не может остаться Planned.
    assert_no_planned_status(&documents.s42_acceptance, "S42 acceptance");
}

/// Typed role schema и compositional semantics не позволяют подменять границы друг другом.
#[test]
fn row_role_schema_and_semantics_are_exact() {
    // Загружаем scoped S42 schema owner.
    let s42_acceptance = load_json_document(S42_ACCEPTANCE_PATH);
    // Typed role schema сохраняет exact читаемый порядок.
    assert_eq!(
        required_string_array(&s42_acceptance, "row_role_schema"),
        REQUIRED_ROLES
    );
    // Compositional semantics запрещает выдавать один fixture за полный codec E2E.
    let role_semantics = required_object(&s42_acceptance, "row_role_semantics");
    // Semantics обязана описывать те же пять ролей без скрытого шестого axis.
    assert_eq!(
        role_semantics
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        REQUIRED_ROLES.into_iter().collect::<BTreeSet<_>>()
    );
    // Runtime fixture не подменяет provider/demux/decoder roles.
    assert_eq!(
        role_semantics
            .get("runtime_fixture")
            .and_then(Value::as_str),
        Some(
            "focused hermetic runtime behavior associated with the row; it does not alone prove the provider boundary, demux boundary, or end-to-end decode of every codec family named by the profile row"
        )
    );
    // Decoder role отдельно ссылается на shared executable registry evidence.
    assert_eq!(
        role_semantics.get("decoder").and_then(Value::as_str),
        Some(
            "shared executable decoder registry evidence; it does not claim that every codec family is present in every runtime fixture"
        )
    );
    // Capability semantics различает positive matrix и supplemental negative gate.
    assert_eq!(
        role_semantics.get("capability").and_then(Value::as_str),
        Some(
            "shared executable fail-closed capability-intersection evidence; negative-only evidence remains incomplete until a positive per-row matrix test is checked in"
        )
    );
}

/// S42 rows сохраняют exact S00 identity и immutable S41 status handoff.
#[test]
fn canonical_rows_match_profile_and_s41_handoff() {
    // Загружаем три immutable traceability документа.
    let documents = TraceabilityDocuments::load();
    // Canonical rows индексируются отдельно от runtime documents.
    let profile_rows = documents.profile_rows();
    // S41 row set остаётся immutable handoff.
    let s41_rows = documents.s41_rows();
    // S42 row set обязан быть exact one-to-one projection.
    let s42_rows = documents.s42_rows();
    // Exact count защищает approved inventory от silent shrink/expansion.
    assert_eq!(profile_rows.len(), APPROVED_ROW_COUNT);
    // S41 и S00 обязаны по-прежнему совпадать.
    assert_eq!(
        profile_rows.keys().copied().collect::<BTreeSet<_>>(),
        s41_rows.keys().copied().collect::<BTreeSet<_>>()
    );
    // S42 не может добавить guessed provider row или потерять approved target.
    assert_eq!(
        profile_rows.keys().copied().collect::<BTreeSet<_>>(),
        s42_rows.keys().copied().collect::<BTreeSet<_>>()
    );

    // Счётчик доказывает exact Implemented set.
    let mut implemented_count = 0_usize;
    // Каждая row проверяется независимо.
    for (row_id, s42_row) in s42_rows {
        // S41 matching row уже гарантирована exact set comparison.
        let s41_row = s41_rows
            .get(row_id)
            // Missing row здесь означает внутренне противоречивый gate.
            .expect("matching S41 row обязана существовать");
        // S42 не переписывает уже утверждённый runtime disposition.
        assert_eq!(
            required_string(s42_row, "status"),
            required_string(s41_row, "status"),
            "S41/S42 status расходится для `{row_id}`"
        );
        // Matching canonical row уже доказана exact identity set comparison.
        let profile_row = profile_rows
            .get(row_id)
            // Missing row здесь означала бы internal gate contradiction.
            .expect("matching S00 row обязана существовать");
        // Transport identity копируется exact, без alias normalization.
        assert_eq!(
            required_string(s42_row, "transport"),
            required_string(profile_row, "transport"),
            "canonical transport расходится для `{row_id}`"
        );
        // Container profile остаётся exact S00 boundary.
        assert_eq!(
            required_string(s42_row, "container_profile"),
            required_string(profile_row, "container_profile"),
            "canonical container profile расходится для `{row_id}`"
        );
        // Codec-profile references сохраняют exact order и identities.
        assert_eq!(
            required_string_array(s42_row, "codec_profile_refs"),
            required_string_array(profile_row, "codec_profile_refs"),
            "canonical codec refs расходятся для `{row_id}`"
        );
        // Runtime fixture identity обязана совпасть с canonical corpus row.
        assert_eq!(
            required_string(s42_row, "fixture_id"),
            required_string(profile_row, "fixture_id"),
            "canonical fixture ID расходится для `{row_id}`"
        );
        // Role object обязан содержать exact schema set.
        exact_row_roles(row_id, s42_row);

        // Runtime status определяет допустимый canonical disposition.
        match required_string(s42_row, "status") {
            // Implemented row входит в exact positive count.
            "Implemented" => {
                // Aggregate RTMP нельзя ошибочно принять как provider.
                assert_ne!(row_id, RTMP_IDENTITY_ONLY_ROW);
                // Exact implemented count увеличивается один раз на row.
                implemented_count += 1;
            }
            // Единственный excluded target — aggregate RTMP identity.
            "ProfileExcluded" => {
                // Любая другая target row была бы Implemented gap.
                assert_eq!(row_id, RTMP_IDENTITY_ONLY_ROW);
            }
            // Любой новый status требует отдельного architecture/profile решения.
            unexpected_status => {
                panic!("недопустимый S42 row status `{unexpected_status}` для `{row_id}`")
            }
        }
    }
    // Exact count не позволяет скрыть Implemented gap.
    assert_eq!(implemented_count, IMPLEMENTED_ROW_COUNT);
}

/// Каждая Implemented row имеет реальное typed evidence для всех пяти ролей.
#[test]
fn implemented_rows_have_complete_typed_role_evidence() {
    // Загружаем scoped S42 acceptance owner.
    let s42_acceptance = load_json_document(S42_ACCEPTANCE_PATH);
    // Catalog нужен для role-compatibility assertions каждой row.
    let evidence_catalog = required_object(&s42_acceptance, "evidence_catalog");
    // S42 rows индексируются по stable identity.
    let s42_rows = rows_by_id(required_array(&s42_acceptance, "rows"), "S42 acceptance");
    // Локальный count не позволяет test filter-у скрыть missing Implemented row.
    let mut implemented_count = 0_usize;

    // Каждая row получает самостоятельную typed проверку.
    for (row_id, s42_row) in s42_rows {
        // Excluded aggregate RTMP проверяется отдельным focused test.
        if required_string(s42_row, "status") != "Implemented" {
            // Неположительная row здесь не создаёт fake role evidence.
            continue;
        }
        // Aggregate RTMP нельзя ошибочно принять как provider.
        assert_ne!(row_id, RTMP_IDENTITY_ONLY_ROW);
        // Exact implemented count увеличивается один раз на row.
        implemented_count += 1;
        // Role object обязан содержать exact schema set.
        let roles = exact_row_roles(row_id, s42_row);

        // Все пять ролей обязательны.
        for role_name in REQUIRED_ROLES {
            // Role value обязан быть object.
            let role = roles
                .get(role_name)
                // Missing role уже пойман set assertion, но diagnostic остаётся точным.
                .and_then(Value::as_object)
                // Неверный type не допускается.
                .unwrap_or_else(|| panic!("role `{role_name}` для `{row_id}` обязан быть object"));
            // Object helper ожидает Value, поэтому берём исходный field.
            let role_value = roles
                .get(role_name)
                // Field доказан выше.
                .expect("matching role value обязан существовать");
            // Implemented role не допускает nullable/fake disposition.
            assert_eq!(
                required_string(role_value, "disposition"),
                "Evidence",
                "Implemented `{row_id}` role `{role_name}` не имеет evidence"
            );
            // Каждая Implemented role обязана ссылаться минимум на одно evidence.
            let role_evidence_ids = required_evidence_ids(
                role_value,
                "evidence_ids",
                &format!("`{row_id}` role `{role_name}`"),
            );
            // Empty Implemented role запрещён.
            assert!(
                !role_evidence_ids.is_empty(),
                "Implemented `{row_id}` role `{role_name}` имеет пустое evidence"
            );
            // Catalog role не может подменить provider test decoder test-ом.
            assert_evidence_role(
                evidence_catalog,
                &role_evidence_ids,
                role_name,
                &format!("`{row_id}` role `{role_name}`"),
            );
            // Capability role требует positive matrix и supplemental negative rejection.
            if role_name == "capability" {
                // Полярности проверяются только у executable evidence.
                assert_eq!(
                    executable_polarities(evidence_catalog, &role_evidence_ids),
                    BTreeSet::from(["Positive", "Negative"]),
                    "`{row_id}` capability должна иметь positive и negative evidence"
                );
            }
            // Использование object выше закрепляет его JSON type.
            assert!(!role.is_empty());
        }
    }
    // Exact count не позволяет filter-у скрыть Implemented gap.
    assert_eq!(implemented_count, IMPLEMENTED_ROW_COUNT);
}

/// Aggregate RTMP row остаётся identity-only exclusion с exact wire inventory.
#[test]
fn aggregate_rtmp_row_remains_explicit_profile_exclusion() {
    // Загружаем canonical и scoped traceability artifacts.
    let documents = TraceabilityDocuments::load();
    // Canonical exclusions нужны для exact RTMP semantic cross-check.
    let profile_excluded_rows = documents.profile_excluded_rows();
    // Scoped rows индексируются по stable identity.
    let s42_rows = documents.s42_rows();
    // Evidence catalog валидирует typed provider/runtime exclusion roles.
    let evidence_catalog = required_object(&documents.s42_acceptance, "evidence_catalog");
    // Aggregate RTMP row обязана существовать в exact target inventory.
    let rtmp_row = s42_rows
        .get(RTMP_IDENTITY_ONLY_ROW)
        // Missing aggregate row был бы Implemented/exclusion gap.
        .expect("aggregate RTMP row обязана существовать");
    // Runtime disposition остаётся typed exclusion.
    assert_eq!(required_string(rtmp_row, "status"), "ProfileExcluded");
    // Role schema остаётся общей для всех target rows.
    let roles = exact_row_roles(RTMP_IDENTITY_ONLY_ROW, rtmp_row);
    // Aggregate identity обязана перечислить exact wire exclusions.
    let exact_rtmp_exclusions = required_string_array(rtmp_row, "exact_profile_exclusion_ids");
    // Exact set не позволяет забыть plain/encrypted/tunnel/TLS variants.
    assert_eq!(
        exact_rtmp_exclusions
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "rtmp-plain-wire",
            "rtmpe-encrypted-wire",
            "rtmp-ffmpeg-pseudo-protocol",
            "rtmps-tls-wire",
            "rtmpt-http-tunnel-wire",
            "rtmpte-encrypted-http-tunnel-wire",
        ])
    );
    // Каждая exact identity обязана реально оставаться canonical exclusion.
    for exclusion_id in exact_rtmp_exclusions {
        // Matching profile row должна существовать.
        let exclusion = profile_excluded_rows
            .get(exclusion_id)
            // Missing exact row превращал бы aggregate claim в guess.
            .unwrap_or_else(|| panic!("canonical RTMP exclusion `{exclusion_id}` отсутствует"));
        // Hard и provisional exclusions допустимы, Implemented/Planned — нет.
        assert!(matches!(
            required_string(exclusion, "status"),
            "ProfileExcluded" | "ProfileExcludedProvisional"
        ));
    }

    // Provider и runtime fixture обязаны ссылаться на exclusion evidence.
    for role_name in ["provider", "runtime_fixture"] {
        // Exact role value уже существует.
        let role_value = roles
            .get(role_name)
            // Missing role уже пойман exact set assertion.
            .expect("excluded role value обязан существовать");
        // Exclusion остаётся typed, а не false Implemented.
        assert_eq!(
            required_string(role_value, "disposition"),
            "ProfileExcluded"
        );
        // Exclusion без checked-in evidence запрещена.
        let exclusion_evidence_ids = required_evidence_ids(
            role_value,
            "evidence_ids",
            &format!("`{}` role `{role_name}`", RTMP_IDENTITY_ONLY_ROW),
        );
        // Exclusion без evidence запрещена.
        assert!(!exclusion_evidence_ids.is_empty());
        // Даже exclusion evidence обязано соответствовать role.
        assert_evidence_role(
            evidence_catalog,
            &exclusion_evidence_ids,
            role_name,
            &format!("`{}` role `{role_name}`", RTMP_IDENTITY_ONLY_ROW),
        );
    }

    // Без wire provider downstream roles честно NotApplicable.
    for role_name in ["demux", "decoder", "capability"] {
        // Exact role value уже существует.
        let role_value = roles
            .get(role_name)
            // Missing role уже пойман exact set assertion.
            .expect("not-applicable role value обязан существовать");
        // NotApplicable нельзя выдать за Implemented evidence.
        assert_eq!(required_string(role_value, "disposition"), "NotApplicable");
        // NotApplicable не должно ссылаться на фиктивный provider test.
        assert!(
            required_evidence_ids(
                role_value,
                "evidence_ids",
                &format!("`{}` role `{role_name}`", RTMP_IDENTITY_ONLY_ROW),
            )
            .is_empty()
        );
    }
}
