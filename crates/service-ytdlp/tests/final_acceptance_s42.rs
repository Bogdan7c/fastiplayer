//! Hermetic S42 gate для утверждённого profile goal→code/tests traceability.
//!
//! S00 остаётся immutable compatibility inventory, S41 остаётся runtime coverage
//! handoff, а этот test target не выдаёт scoped traceability за full/manual acceptance.

// Общий helper facade владеет JSON shape, evidence source и filesystem assertions.
#[path = "final_acceptance_s42/support.rs"]
mod support;
// One-to-one S00/S41/S42 row traceability имеет отдельного focused owner-а.
#[path = "final_acceptance_s42/profile_traceability.rs"]
mod profile_traceability;
// Evidence catalog integrity не смешивается с profile row semantics.
#[path = "final_acceptance_s42/evidence_catalog.rs"]
mod evidence_catalog;
// Release exclusions и approved extended absences проверяются отдельно.
#[path = "final_acceptance_s42/release_scope.rs"]
mod release_scope;
// Cross-cutting security/lifecycle proofs имеют отдельного owner-а.
#[path = "final_acceptance_s42/cross_cutting.rs"]
mod cross_cutting;
// Public DASH-live runtime evidence имеет отдельного owner-а.
#[path = "final_acceptance_s42/dash_live.rs"]
mod dash_live;
// Canonical profile/S41/S42 Planned-status inventory имеет отдельный focused gate.
#[path = "final_acceptance_s42/status_inventory.rs"]
mod status_inventory;
// Quality polarity и owner-approved hardware delta проверяются отдельным owner-ом.
#[path = "final_acceptance_s42/quality_hardware.rs"]
mod quality_hardware;
// Полный §14/release roadmap trace проверяется отдельным focused owner-ом.
#[path = "final_acceptance_s42/roadmap_trace.rs"]
mod roadmap_trace;

// Canonical S00 profile не переписывается финальным acceptance gate.
const PROFILE_PATH: &str = "compatibility/2026.07.04/profile.json";
// S41 runtime disposition остаётся отдельным предыдущим handoff.
const S41_COVERAGE_PATH: &str = "compatibility/2026.07.04/runtime-coverage-s41.json";
// S42 хранит только scoped profile traceability и owner-approved exception.
const S42_ACCEPTANCE_PATH: &str = "compatibility/2026.07.04/final-acceptance-s42.json";
// Exact approved inventory содержит тринадцать canonical target rows.
const APPROVED_ROW_COUNT: usize = 13;
// Двенадцать exact rows имеют production path.
const IMPLEMENTED_ROW_COUNT: usize = 12;
// Aggregate RTMP identity остаётся единственной исключённой target row.
const RTMP_IDENTITY_ONLY_ROW: &str = "rtmp-family-flv";
// Каждая row обязана явно закрыть одинаковый typed role set.
const REQUIRED_ROLES: [&str; 5] = [
    "provider",
    "demux",
    "decoder",
    "runtime_fixture",
    "capability",
];
