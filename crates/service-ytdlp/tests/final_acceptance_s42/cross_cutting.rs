//! Exact S42 traceability gate для общих security/lifecycle/rollback инвариантов.
//!
//! Этот модуль не доказывает весь release acceptance. Он связывает только пять
//! cross-cutting требования с реально исполняемыми checked-in тестами.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::support::{
    assert_typed_evidence, load_json_document, required_evidence_ids, required_object,
    required_string,
};

/// Путь к scoped S42 profile-traceability manifest относительно service-ytdlp.
const S42_ACCEPTANCE_PATH: &str = "compatibility/2026.07.04/final-acceptance-s42.json";

/// Точная ожидаемая запись одного исполняемого cross-cutting доказательства.
#[derive(Clone, Copy)]
struct ExpectedExecutableEvidence {
    /// Stable catalog ID, на который ссылается requirement group.
    id: &'static str,
    /// Positive success proof либо Negative adversarial/failure proof.
    polarity: &'static str,
    /// Cargo package, исполняющий test symbol.
    package: &'static str,
    /// Cargo target: `lib` либо имя integration-test target.
    target: &'static str,
    /// Точный checked-in source path.
    path: &'static str,
    /// Точное имя функции с непосредственным `#[test]`.
    symbol: &'static str,
}

/// Secret/auth evidence проверяет transport forwarding и безопасные diagnostics.
const SECRET_AND_AUTH_EVIDENCE: &[ExpectedExecutableEvidence] = &[
    ExpectedExecutableEvidence {
        id: "cross-secret-auth-redirect",
        polarity: "Negative",
        package: "web-media-http",
        target: "lib",
        path: "crates/web-media-http/src/tests.rs",
        symbol: "cross_origin_redirect_never_forwards_authorization_header",
    },
    ExpectedExecutableEvidence {
        id: "cross-secret-auth-redaction",
        polarity: "Negative",
        package: "web-media-http",
        target: "lib",
        path: "crates/web-media-http/src/tests.rs",
        symbol: "auth_errors_and_debug_output_redact_header_value_and_url_payload",
    },
    ExpectedExecutableEvidence {
        id: "cross-secret-range-redirect",
        polarity: "Negative",
        package: "web-media-http",
        target: "lib",
        path: "crates/web-media-http/src/range_redirect_tests.rs",
        symbol: "cross_origin_range_redirect_prefetches_without_forwarding_secrets",
    },
];

/// Durable locator сохраняется exact, а transient request material не имеет DTO shape.
const LOCATOR_AND_TRANSIENT_SECRET_EVIDENCE: &[ExpectedExecutableEvidence] = &[
    ExpectedExecutableEvidence {
        id: "cross-acknowledged-locator-roundtrip",
        polarity: "Positive",
        package: "playlist-state",
        target: "lib",
        path: "crates/playlist-state/src/v2_tests.rs",
        symbol: "v2_roundtrip_preserves_top_level_order_current_shuffle_allocators_and_payloads",
    },
    ExpectedExecutableEvidence {
        id: "cross-transient-request-material-unrepresentable",
        polarity: "Negative",
        package: "playlist-state",
        target: "lib",
        path: "crates/playlist-state/src/v2_tests.rs",
        symbol: "transient_request_material_is_structurally_unrepresentable_in_v2_dto",
    },
];

/// Cancellation проходит от app owner-а к transport, stale generation не делает I/O.
const CANCELLATION_AND_STALE_EVIDENCE: &[ExpectedExecutableEvidence] = &[
    ExpectedExecutableEvidence {
        id: "cross-cancellation-source-token",
        polarity: "Negative",
        package: "app-egui",
        target: "lib",
        path: "crates/app-egui/src/media_open/executor.rs",
        symbol: "cancellation_reaches_source_transport_token",
    },
    ExpectedExecutableEvidence {
        id: "cross-stale-generation-no-network",
        polarity: "Negative",
        package: "web-media-adaptive",
        target: "lib",
        path: "crates/web-media-adaptive/src/tests/range_source.rs",
        symbol: "range_source_rejects_stale_generation_and_cancel_before_network",
    },
];

/// Bounded shutdown не теряет JoinHandle после timeout и затем reap-ит worker.
const BOUNDED_SHUTDOWN_EVIDENCE: &[ExpectedExecutableEvidence] = &[ExpectedExecutableEvidence {
    id: "cross-bounded-shutdown-reap",
    polarity: "Negative",
    package: "app-egui",
    target: "lib",
    path: "crates/app-egui/src/media_open/executor.rs",
    symbol: "timeout_retains_worker_handle_and_later_reaps_it",
}];

/// Import/open/switch pre-barrier failures не публикуют разрушительное состояние.
const FAILED_PRE_BARRIER_EVIDENCE: &[ExpectedExecutableEvidence] = &[
    ExpectedExecutableEvidence {
        id: "cross-pre-barrier-import-active-preserved",
        polarity: "Negative",
        package: "app-egui",
        target: "lib",
        path: "crates/app-egui/src/playlist_runtime/import_transaction/tests.rs",
        symbol: "partial_preview_is_bounded_and_cancelled_confirmation_is_mutation_free",
    },
    ExpectedExecutableEvidence {
        id: "cross-pre-barrier-import-stale-failure",
        polarity: "Negative",
        package: "app-egui",
        target: "lib",
        path: "crates/app-egui/src/playlist_runtime/import_transaction/tests.rs",
        symbol: "structural_stale_failure_preserves_import_ids_and_queue",
    },
    ExpectedExecutableEvidence {
        id: "cross-pre-barrier-open-pipeline-preserved",
        polarity: "Negative",
        package: "player-core",
        target: "lib",
        path: "crates/player-core/src/session/tests/staged_media_install.rs",
        symbol: "resource_and_configuration_failures_are_pre_ready_and_preserve_old_playing",
    },
    ExpectedExecutableEvidence {
        id: "cross-pre-barrier-switch-player-preserved",
        polarity: "Negative",
        package: "player-core",
        target: "lib",
        path: "crates/player-core/src/session/tests/live_same_item_restore.rs",
        symbol: "cancelled_pre_barrier_candidate_releases_only_new_generation_once",
    },
    ExpectedExecutableEvidence {
        id: "cross-pre-barrier-switch-selector-restored",
        polarity: "Negative",
        package: "app-egui",
        target: "lib",
        path: "crates/app-egui/src/web_media_stream_model/tests.rs",
        symbol: "candidate_switch_selector_is_single_flight_and_pre_barrier_failure_restores_it",
    },
];

/// Точные requirement group names и их единственный допустимый evidence set.
const CROSS_CUTTING_GROUPS: &[(&str, &[ExpectedExecutableEvidence])] = &[
    ("secret_and_auth_non_leakage", SECRET_AND_AUTH_EVIDENCE),
    (
        "acknowledged_locator_and_transient_secret_persistence",
        LOCATOR_AND_TRANSIENT_SECRET_EVIDENCE,
    ),
    (
        "cancellation_and_stale_generation",
        CANCELLATION_AND_STALE_EVIDENCE,
    ),
    ("bounded_shutdown", BOUNDED_SHUTDOWN_EVIDENCE),
    (
        "failed_pre_barrier_preservation",
        FAILED_PRE_BARRIER_EVIDENCE,
    ),
];

/// Возвращает catalog entry по exact ID без fallback/substring matching.
fn required_catalog_entry<'catalog>(
    catalog: &'catalog Map<String, Value>,
    evidence_id: &str,
) -> &'catalog Value {
    catalog
        .get(evidence_id)
        .unwrap_or_else(|| panic!("отсутствует exact evidence catalog ID `{evidence_id}`"))
}

/// Собирает exact множество ID из одного статического requirement specification.
fn expected_evidence_ids(
    expected_evidence: &[ExpectedExecutableEvidence],
) -> BTreeSet<&'static str> {
    expected_evidence
        .iter()
        .map(|evidence| evidence.id)
        .collect()
}

/// Проверяет все typed поля и exact source binding catalog entry.
fn assert_exact_executable_evidence(
    catalog: &Map<String, Value>,
    expected: ExpectedExecutableEvidence,
) {
    let evidence = required_catalog_entry(catalog, expected.id);

    // Общий validator доказывает kind-specific shape, Cargo target и реальный #[test].
    assert_typed_evidence(expected.id, evidence);
    // Cross-cutting role не смешивается с provider/demux/quality evidence.
    assert_eq!(required_string(evidence, "kind"), "ExecutableTest");
    assert_eq!(required_string(evidence, "role"), "cross_cutting");
    // Success и adversarial proofs различаются явно, без вывода из имени test-а.
    assert_eq!(required_string(evidence, "polarity"), expected.polarity);
    // Exact equality запрещает тихо перенести proof в другой package/target/source.
    assert_eq!(required_string(evidence, "package"), expected.package);
    assert_eq!(required_string(evidence, "target"), expected.target);
    assert_eq!(required_string(evidence, "path"), expected.path);
    assert_eq!(required_string(evidence, "symbol"), expected.symbol);
}

/// S42 cross-cutting manifest содержит ровно утверждённые executable proofs.
#[test]
fn cross_cutting_requirements_bind_exact_executable_tests() {
    let acceptance = load_json_document(S42_ACCEPTANCE_PATH);
    let cross_cutting = required_object(&acceptance, "cross_cutting_evidence");
    let catalog = required_object(&acceptance, "evidence_catalog");

    // Новый requirement требует осознанного изменения этого exact schema set.
    assert_eq!(
        cross_cutting
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        CROSS_CUTTING_GROUPS
            .iter()
            .map(|(requirement, _)| *requirement)
            .collect::<BTreeSet<_>>()
    );

    let mut all_expected_ids = BTreeSet::new();
    for (requirement, expected_evidence) in CROSS_CUTTING_GROUPS {
        let requirement_value = cross_cutting
            .get(*requirement)
            .unwrap_or_else(|| panic!("отсутствует cross-cutting requirement `{requirement}`"));
        // Requirement object допускает только explicit evidence_ids.
        assert_eq!(
            requirement_value
                .as_object()
                .expect("cross-cutting requirement обязан быть object")
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["evidence_ids"])
        );
        let declared_ids = required_evidence_ids(
            requirement_value,
            "evidence_ids",
            &format!("cross-cutting requirement `{requirement}`"),
        )
        .into_iter()
        .collect::<BTreeSet<_>>();
        let expected_ids = expected_evidence_ids(expected_evidence);
        assert_eq!(
            declared_ids, expected_ids,
            "cross-cutting requirement `{requirement}` изменил exact evidence set"
        );

        for expected in *expected_evidence {
            assert!(
                all_expected_ids.insert(expected.id),
                "evidence ID `{}` повторно используется между requirement groups",
                expected.id
            );
            assert_exact_executable_evidence(catalog, *expected);
        }
    }

    // Catalog role-set exact: скрытое cross-cutting evidence не останется без requirement.
    let catalog_cross_cutting_ids = catalog
        .iter()
        .filter_map(|(evidence_id, evidence)| {
            (evidence.get("role").and_then(Value::as_str) == Some("cross_cutting"))
                .then_some(evidence_id.as_str())
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(catalog_cross_cutting_ids, all_expected_ids);
}
