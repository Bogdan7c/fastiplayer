//! Строгий machine-readable trace всех hermetic пунктов S42 §14 и release audits.

// Ordered set делает ratchet и diagnostics детерминированными.
use std::collections::BTreeSet;
// Filesystem нужен только для checked-in Python/shell evidence.
use std::fs;
// Path проверяет target identity без зависимости от текущего cwd.
use std::path::Path;

// JSON остаётся внешним versioned artifact, поэтому validator работает с explicit Value.
use serde_json::Value;

// Общие S42 helpers не дублируют Cargo package/test validation.
use super::support::{
    assert_typed_evidence, load_json_document, required_array, required_object, required_string,
    workspace_root,
};

/// Путь к полному roadmap trace относительно `service-ytdlp`.
const ROADMAP_TRACE_PATH: &str = "compatibility/2026.07.04/roadmap-trace-s42.json";

/// Exact 31 hermetic IDs из §14 плюс 15 mandatory audits и manual non-automation.
const EXPECTED_REQUIREMENT_IDS: [&str; 47] = [
    "s14-01-playlist-state-v1-v2",
    "s14-02-compound-storage-navigation-ui-mpris",
    "s14-03-playlist-formats",
    "s14-04-hls-playlist-classification",
    "s14-05-non-utf-import-export",
    "s14-06-nested-cycles-budgets",
    "s14-07-import-confirmations",
    "s14-08-export-durability",
    "s14-09-toolbar-artwork-accessibility-layout",
    "s14-10-topology-shapes",
    "s14-11-topology-identity-cases",
    "s14-12-profile-schema-raw-unknown",
    "s14-13-request-material-shapes",
    "s14-14-layout-shapes",
    "s14-15-height-preference-runtime-override",
    "s14-16-http-range-non-range-refresh",
    "s14-17-demux-registry-composite-av",
    "s14-18-temporary-readiness-retry-fencing",
    "s14-19-current-container-regression",
    "s14-20-mpeg-ts-flv",
    "s14-21-progressive-hls-dash",
    "s14-22-approved-extended-provider-status",
    "s14-23-approved-extended-absence-evidence",
    "s14-24-dynamic-live-dvr",
    "s14-25-auth-scope-non-leakage",
    "s14-26-locator-transient-persistence",
    "s14-27-cue-playback-window",
    "s14-28-exact-restore-receipt",
    "s14-29-candidate-switch-barrier",
    "s14-30-secrets-cancel-stale-shutdown",
    "s14-31-ffmpeg-boundary",
    "audit-01-goal-code-tests-traceability",
    "audit-02-no-implemented-gap",
    "audit-03-no-planned-row",
    "audit-04-explicit-release-exclusions",
    "audit-05-full-hermetic-suites",
    "audit-06-clippy-rustdoc-fmt",
    "audit-07-rust-toolchains",
    "audit-08-dependency-coverage-inventory",
    "audit-09-guardrails-module-sizes",
    "audit-10-secret-cancel-stale-shutdown",
    "audit-11-hardware-capability-exception",
    "audit-12-ffmpeg-decode-only",
    "audit-13-parser-ownership",
    "audit-14-http-cache-prefetch-webm-ownership",
    "audit-15-xml-advisory-graph",
    "audit-16-manual-opt-in-not-automated",
];

/// Exact provider-status evidence запрещает fake RTMP/special-provider positive proof.
const EXTENDED_PROVIDER_EVIDENCE: [&str; 5] = [
    "smooth-runtime",
    "ftp-runtime",
    "hds-runtime",
    "rtmp-profile-excluded",
    "special-provider-no-approved-row",
];

/// Exact F4F exception evidence связывает принятое решение с fail-closed relocation test.
const PARSER_OWNERSHIP_EVIDENCE: [&str; 3] = [
    "guardrail-duplicate-parser-cache-encoder",
    "guardrail-f4f-exact-adapter",
    "guardrail-f4f-relocation",
];

/// Возвращает строковое множество без permissive JSON coercion.
fn exact_string_set(values: &[Value], context: &str) -> BTreeSet<String> {
    // Результат одновременно проверяет uniqueness.
    let mut result = BTreeSet::new();
    // Каждый элемент обязан быть непустой строкой.
    for value in values {
        // Неверный JSON type является schema failure.
        let text = value
            .as_str()
            // Context называет точное место malformed artifact.
            .unwrap_or_else(|| panic!("{context} содержит нестроковое значение"));
        // Пустая identity не может участвовать в trace.
        assert!(!text.is_empty(), "{context} содержит пустую identity");
        // Duplicate evidence/requirement identity запрещён.
        assert!(
            result.insert(text.to_owned()),
            "{context} содержит duplicate `{text}`"
        );
    }
    // Owned set не удерживает borrow JSON document-а.
    result
}

/// Находит exact requirement row без fallback на похожий ID.
fn requirement_by_id<'document>(document: &'document Value, expected_id: &str) -> &'document Value {
    // Requirements сохраняют human-readable order §14.
    required_array(document, "requirements")
        .iter()
        // Exact byte equality запрещает alias и prefix matching.
        .find(|requirement| required_string(requirement, "id") == expected_id)
        // Missing ratcheted row немедленно ломает gate.
        .unwrap_or_else(|| panic!("roadmap trace не содержит requirement `{expected_id}`"))
}

/// Возвращает exact evidence IDs одного requirement.
fn requirement_evidence_ids(document: &Value, requirement_id: &str) -> BTreeSet<String> {
    // Сначала находим exact row.
    let requirement = requirement_by_id(document, requirement_id);
    // Затем проверяем typed non-empty evidence list.
    let evidence_ids = exact_string_set(
        required_array(requirement, "evidence_ids"),
        &format!("requirement `{requirement_id}` evidence_ids"),
    );
    // Пустая ссылка не является goal→tests trace.
    assert!(
        !evidence_ids.is_empty(),
        "requirement `{requirement_id}` не имеет evidence"
    );
    // Caller получает canonical set.
    evidence_ids
}

/// Проверяет exact schema одного requirement row.
fn assert_requirement_schema(requirement: &Value) {
    // Row обязан быть JSON object.
    let requirement_object = requirement
        .as_object()
        // Array scalar не может пройти schema.
        .expect("roadmap requirement обязан быть object");
    // Version 1 допускает только три explicit fields.
    let actual_fields = requirement_object
        .keys()
        // Borrowed names достаточно для сравнения.
        .map(String::as_str)
        // Ordered set стабилизирует failure diff.
        .collect::<BTreeSet<_>>();
    // Unknown field требует schema bump и validator update.
    assert_eq!(
        actual_fields,
        BTreeSet::from(["id", "goal", "evidence_ids"]),
        "roadmap requirement schema изменился без version bump"
    );
    // Human-readable goal не заменяет stable ID, но не может быть пустым.
    assert!(
        !required_string(requirement, "goal").is_empty(),
        "roadmap requirement содержит пустой goal"
    );
}

/// Читает checked-in source и проверяет common path/symbol invariants.
fn source_with_symbol(evidence_id: &str, evidence: &Value) -> String {
    // Workspace-relative path запрещает cwd-dependent trace.
    let relative_path = required_string(evidence, "path");
    // Absolute и parent traversal paths не допускаются.
    let parsed_path = Path::new(relative_path);
    // Evidence всегда остаётся внутри workspace.
    assert!(
        !parsed_path.is_absolute()
            && !parsed_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
        "evidence `{evidence_id}` содержит небезопасный path"
    );
    // Source читается из canonical workspace root.
    let source_path = workspace_root().join(parsed_path);
    // Missing source является stale trace, а не skipped evidence.
    let source = fs::read_to_string(&source_path).unwrap_or_else(|error| {
        panic!(
            "evidence `{evidence_id}` не удалось прочитать {}: {error}",
            source_path.display()
        )
    });
    // Exact symbol не допускает пустой anchor.
    let symbol = required_string(evidence, "symbol");
    // Подстрока является только common preflight; kind-specific checks идут ниже.
    assert!(
        !symbol.is_empty() && source.contains(symbol),
        "evidence `{evidence_id}` не содержит symbol `{symbol}`"
    );
    // Source возвращается kind-specific validator-у.
    source
}

/// Проверяет executable Python unittest method и owner class.
fn assert_python_unittest(evidence_id: &str, evidence: &Value) {
    // Common source/path/symbol validation выполняется первым.
    let source = source_with_symbol(evidence_id, evidence);
    // Python evidence имеет exact versioned shape.
    let actual_fields = evidence
        .as_object()
        // Catalog entry обязан быть object.
        .expect("Python evidence обязан быть object")
        .keys()
        // Field names сравниваются без allocation.
        .map(String::as_str)
        // Ordered set делает schema error стабильным.
        .collect::<BTreeSet<_>>();
    // Package/polarity не выдумываются для Python runner-а.
    assert_eq!(
        actual_fields,
        BTreeSet::from(["kind", "role", "target", "path", "symbol"])
    );
    // Только checked-in unittest files допустимы как Python proof.
    let path = required_string(evidence, "path");
    // Exact directory исключает arbitrary tool source без теста.
    assert!(
        path.starts_with("scripts/tests/test_") && path.ends_with(".py"),
        "Python evidence `{evidence_id}` не является checked-in unittest"
    );
    // Target является unittest.TestCase owner class.
    let target = required_string(evidence, "target");
    // Exact class declaration связывает symbol с executable discovery owner-ом.
    assert!(
        source.contains(&format!("class {target}(unittest.TestCase):")),
        "Python evidence `{evidence_id}` не содержит target class `{target}`"
    );
    // Method обязан быть discoverable standard unittest test.
    let symbol = required_string(evidence, "symbol");
    // Prefix не позволяет выдать helper за executable test.
    assert!(
        symbol.starts_with("test_")
            && source.contains(&format!("    def {symbol}(self"))
            && source.contains("unittest.main()"),
        "Python evidence `{evidence_id}` symbol `{symbol}` не является unittest test"
    );
}

/// Проверяет exact shell function, которой владеет release runner.
fn assert_script_function(evidence_id: &str, evidence: &Value) {
    // Common source/path/symbol validation выполняется первым.
    let source = source_with_symbol(evidence_id, evidence);
    // Shell evidence имеет только executable owner metadata.
    let actual_fields = evidence
        .as_object()
        // Scalar не может пройти kind validator.
        .expect("ScriptFunction evidence обязан быть object")
        .keys()
        // Borrowed field names достаточно.
        .map(String::as_str)
        // Ordered schema comparison.
        .collect::<BTreeSet<_>>();
    // Unknown field требует schema bump.
    assert_eq!(
        actual_fields,
        BTreeSet::from(["kind", "role", "target", "path", "symbol"])
    );
    // Только два checked-in release owners разрешены version 1 trace.
    let path = required_string(evidence, "path");
    // Target явно различает CI и coverage runners.
    let expected_target = match path {
        // Main CI owner.
        "scripts/ci-checks.sh" => "ci-checks",
        // Coverage owner.
        "scripts/coverage.sh" => "coverage",
        // Новый script требует обсуждаемого schema update.
        _ => panic!("ScriptFunction evidence `{evidence_id}` имеет unknown owner `{path}`"),
    };
    // Artifact target обязан совпасть с exact owner.
    assert_eq!(required_string(evidence, "target"), expected_target);
    // Exact POSIX-style declaration запрещает совпадение только в callsite/comment.
    let symbol = required_string(evidence, "symbol");
    // Function declaration является исполняемым anchor.
    assert!(
        source
            .lines()
            .any(|line| line.trim() == format!("{symbol}() {{")),
        "ScriptFunction evidence `{evidence_id}` не содержит function `{symbol}`"
    );
}

/// Проверяет kind-specific executable evidence.
fn assert_roadmap_evidence(evidence_id: &str, evidence: &Value) {
    // Каждый evidence сохраняет explicit role.
    assert_eq!(
        required_string(evidence, "role"),
        match required_string(evidence, "kind") {
            // Rust tests используют существующий строгий validator.
            "ExecutableTest" => {
                // Helper проверяет path, exact Cargo package/target и real #[test] fn.
                assert_typed_evidence(evidence_id, evidence);
                // Existing schema допускает несколько semantic roles.
                required_string(evidence, "role")
            }
            // Python test остаётся executable proof, но имеет не-Cargo target.
            "PythonUnittest" => {
                // Focused validator запрещает helper/manual evidence.
                assert_python_unittest(evidence_id, evidence);
                // Release tooling является cross-cutting owner-ом.
                "cross_cutting"
            }
            // Shell owner связывает test-ratcheted command с реальной function.
            "ScriptFunction" => {
                // Function/path/target проверяются exact.
                assert_script_function(evidence_id, evidence);
                // Release tooling также cross-cutting.
                "cross_cutting"
            }
            // Docs/manual/decision evidence не доказывает hermetic goal.
            unexpected_kind =>
                panic!("roadmap evidence `{evidence_id}` имеет unknown kind `{unexpected_kind}`"),
        },
        "roadmap evidence `{evidence_id}` имеет несовместимую role"
    );
}

/// Проверяет complete exact ratchet и каждый checked-in evidence anchor.
#[test]
fn roadmap_trace_covers_exact_s42_requirements_and_real_checked_in_evidence() {
    // Artifact читается через общий cwd-independent helper.
    let document = load_json_document(ROADMAP_TRACE_PATH);
    // Top-level object не допускает silent permissive fields.
    let document_fields = document
        .as_object()
        // Scalar root не может быть roadmap manifest.
        .expect("roadmap trace root обязан быть object")
        .keys()
        // Borrowed keys достаточно для exact schema.
        .map(String::as_str)
        // Ordered set стабилизирует schema diff.
        .collect::<BTreeSet<_>>();
    // Любое новое поле требует schema bump и validator update.
    assert_eq!(
        document_fields,
        BTreeSet::from([
            "schema_version",
            "profile_id",
            "scope",
            "status",
            "requirements",
            "evidence_catalog",
        ])
    );
    // Schema version запрещает silent permissive evolution.
    assert_eq!(document["schema_version"].as_u64(), Some(1));
    // Canonical profile identity связывает trace с immutable S00 inventory.
    assert_eq!(
        required_string(&document, "profile_id"),
        "yt-dlp-2026.07.04-serializable-v1"
    );
    // Scope отделяет полный roadmap trace от row-only profile trace.
    assert_eq!(
        required_string(&document, "scope"),
        "S42RoadmapGoalCodeTestsTrace"
    );
    // Status не выдаёт hermetic trace за manual acceptance.
    assert_eq!(
        required_string(&document, "status"),
        "HermeticTraceCompleteManualAcceptanceNotImplied"
    );
    // Exact requirements array обязателен.
    let requirements = required_array(&document, "requirements");
    // Каждый row проходит versioned schema.
    requirements.iter().for_each(assert_requirement_schema);
    // Artifact IDs собираются с duplicate rejection.
    let actual_requirement_ids = requirements
        .iter()
        // Stable identity берётся только из explicit field.
        .map(|requirement| Value::String(required_string(requirement, "id").to_owned()))
        // Временный Vec нужен общему strict helper-у.
        .collect::<Vec<_>>();
    // Exact set фиксирует все 31 §14 и 16 audit rows, включая manual non-automation audit.
    assert_eq!(
        exact_string_set(&actual_requirement_ids, "requirements"),
        EXPECTED_REQUIREMENT_IDS
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    );

    // Catalog является единственным source metadata owner-ом trace artifact.
    let evidence_catalog = required_object(&document, "evidence_catalog");
    // Used identities одновременно проверяют отсутствие dead catalog rows.
    let mut referenced_evidence_ids = BTreeSet::new();
    // Каждый requirement обязан иметь executable test evidence.
    for requirement in requirements {
        // Stable ID нужен для actionable diagnostic.
        let requirement_id = required_string(requirement, "id");
        // Duplicate evidence внутри row запрещён.
        let evidence_ids = requirement_evidence_ids(&document, requirement_id);
        // Хотя бы один test обязан быть executable Rust/Python test, не только shell anchor.
        let mut has_executable_test = false;
        // Все ссылки проверяются немедленно.
        for evidence_id in evidence_ids {
            // Missing catalog entry является stale trace.
            let evidence = evidence_catalog.get(&evidence_id).unwrap_or_else(|| {
                panic!(
                    "requirement `{requirement_id}` ссылается на missing evidence `{evidence_id}`"
                )
            });
            // Kind-specific source/path/symbol/target проверяется независимо от reuse.
            assert_roadmap_evidence(&evidence_id, evidence);
            // Rust и Python tests являются реальным executable proof.
            has_executable_test |= matches!(
                required_string(evidence, "kind"),
                "ExecutableTest" | "PythonUnittest"
            );
            // Global referenced set используется для dead-row check.
            referenced_evidence_ids.insert(evidence_id);
        }
        // Script-only requirement был бы лишь wiring claim без test ratchet.
        assert!(
            has_executable_test,
            "requirement `{requirement_id}` не имеет executable test evidence"
        );
    }
    // Ни один catalog row не может оставаться stale/dead.
    assert_eq!(
        referenced_evidence_ids,
        evidence_catalog.keys().cloned().collect::<BTreeSet<_>>(),
        "roadmap trace содержит dead либо незаявленное evidence"
    );
}

/// Проверяет два owner-approved места, где ложный positive был бы особенно опасен.
#[test]
fn extended_provider_status_and_f4f_exception_remain_exact_and_honest() {
    // Artifact загружается независимо от первого test.
    let document = load_json_document(ROADMAP_TRACE_PATH);
    // Extended provider row обязан различать positive providers и typed absences.
    assert_eq!(
        requirement_evidence_ids(&document, "s14-22-approved-extended-provider-status"),
        EXTENDED_PROVIDER_EVIDENCE
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    );
    // Catalog нужен для polarity/role checks.
    let evidence_catalog = required_object(&document, "evidence_catalog");
    // RTMP не может стать fake provider proof.
    for excluded_evidence_id in ["rtmp-profile-excluded", "special-provider-no-approved-row"] {
        // Exact catalog row обязателен.
        let evidence = evidence_catalog
            .get(excluded_evidence_id)
            // Missing negative evidence ломает release trace.
            .unwrap_or_else(|| panic!("missing evidence `{excluded_evidence_id}`"));
        // Отрицательная polarity видна machine-readable.
        assert_eq!(required_string(evidence, "polarity"), "Negative");
        // Role остаётся exclusion, а не provider.
        assert_eq!(required_string(evidence, "role"), "exclusion");
    }
    // F4F exception фиксируется вместе с duplicate/relocation fail-closed tests.
    assert_eq!(
        requirement_evidence_ids(&document, "audit-13-parser-ownership"),
        PARSER_OWNERSHIP_EVIDENCE
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    );
}
