//! Recursive Planned-status gate для всей canonical profile handoff chain.

use super::support::{assert_no_planned_status, load_json_document};

/// Canonical approved profile, который release не может greenwash S42 artifact-ом.
const PROFILE_PATH: &str = "compatibility/2026.07.04/profile.json";
/// Предыдущий runtime handoff также обязан остаться без unresolved Planned row.
const S41_COVERAGE_PATH: &str = "compatibility/2026.07.04/runtime-coverage-s41.json";
/// Текущий scoped traceability artifact.
const S42_ACCEPTANCE_PATH: &str = "compatibility/2026.07.04/final-acceptance-s42.json";

/// Canonical profile и оба handoff artifact-а рекурсивно запрещают `Planned`.
#[test]
fn canonical_profile_s41_and_s42_have_no_unresolved_planned_status() {
    for (path, context) in [
        (PROFILE_PATH, "canonical profile"),
        (S41_COVERAGE_PATH, "S41 runtime handoff"),
        (S42_ACCEPTANCE_PATH, "S42 traceability"),
    ] {
        assert_no_planned_status(&load_json_document(path), context);
    }
}
