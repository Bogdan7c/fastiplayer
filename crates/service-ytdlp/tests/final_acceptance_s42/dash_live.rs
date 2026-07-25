//! Exact executable evidence для public DASH-live provider/demux boundary.

use super::support::{
    assert_typed_evidence, load_json_document, required_array, required_evidence_ids,
    required_object, required_string, rows_by_id,
};

/// S42 scoped traceability artifact.
const S42_ACCEPTANCE_PATH: &str = "compatibility/2026.07.04/final-acceptance-s42.json";
/// Canonical dynamic DASH profile row.
const DASH_LIVE_ROW_ID: &str = "dash-live-dvr";
/// Один production-shaped test может типизированно доказывать три разные роли.
const DASH_LIVE_EXECUTABLE_EVIDENCE: [(&str, &str); 3] = [
    ("provider-dash-live-runtime", "provider"),
    ("demux-dash-live-runtime", "demux"),
    ("fixture-dash-live-runtime", "runtime_fixture"),
];
/// Checked-in hermetic test file.
const DASH_LIVE_TEST_PATH: &str = "crates/web-media-dash/tests/live_runtime.rs";
/// Public-boundary test symbol.
const DASH_LIVE_TEST_SYMBOL: &str =
    "prepares_local_dynamic_mpd_until_audio_packet_and_cooperative_shutdown";

/// Dynamic row ссылается на executable public provider, demux и packet evidence.
#[test]
fn dash_live_row_reaches_public_provider_demux_and_packet_fixture() {
    let acceptance = load_json_document(S42_ACCEPTANCE_PATH);
    let evidence_catalog = required_object(&acceptance, "evidence_catalog");
    let rows = rows_by_id(required_array(&acceptance, "rows"), "S42 rows");
    let dash_live_row = rows
        .get(DASH_LIVE_ROW_ID)
        .copied()
        .expect("S42 обязан содержать canonical dash-live-dvr row");
    let roles = required_object(dash_live_row, "roles");

    for (evidence_id, role) in DASH_LIVE_EXECUTABLE_EVIDENCE {
        let evidence = evidence_catalog
            .get(evidence_id)
            .unwrap_or_else(|| panic!("DASH live evidence `{evidence_id}` отсутствует"));
        assert_typed_evidence(evidence_id, evidence);
        assert_eq!(required_string(evidence, "kind"), "ExecutableTest");
        assert_eq!(required_string(evidence, "role"), role);
        assert_eq!(required_string(evidence, "polarity"), "Positive");
        assert_eq!(required_string(evidence, "package"), "web-media-dash");
        assert_eq!(required_string(evidence, "target"), "live_runtime");
        assert_eq!(required_string(evidence, "path"), DASH_LIVE_TEST_PATH);
        assert_eq!(required_string(evidence, "symbol"), DASH_LIVE_TEST_SYMBOL);

        let role_value = roles
            .get(role)
            .unwrap_or_else(|| panic!("DASH live role `{role}` отсутствует"));
        let role_evidence =
            required_evidence_ids(role_value, "evidence_ids", "DASH live executable role");
        assert!(
            role_evidence.contains(&evidence_id),
            "DASH live role `{role}` не ссылается на `{evidence_id}`"
        );
    }
}
