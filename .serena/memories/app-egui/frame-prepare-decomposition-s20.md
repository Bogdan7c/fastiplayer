# Session 20 frame preparation decomposition

- `crates/app-egui/src/frame_prepare.rs` remains the composition coordinator; per-frame input/snapshot I/O moved to `frame_prepare/input_snapshot.rs`, egui FullOutput/platform-output/tessellation and viewport snapshot moved to `frame_prepare/ui_prepare.rs`, and renderer submit/mark-submitted/surface outcome accounting moved to `frame_prepare/submit.rs`.
- Pure egui-point to `render_core::RenderViewport` conversion lives in `frame_prepare/geometry.rs`; player drop/discard to app telemetry classification lives in `frame_prepare/telemetry_mapping.rs`. Focused tests live beside these owners.
- Existing `frame_prepare/shared_frame_materialization.rs` remains the adapter boundary for playback/scrub lease materialization; `AppState` does not acquire new GPU-handle ownership.
- Runtime-sensitive order is guarded by `frame_prepare/sequence.rs`: worker drain -> worker event record/reselection -> desktop publish -> egui output -> materializer lookup -> renderer submit. The production coordinator advances the same debug contract used by a recording fake test; reordered stages panic in debug/test.
- Lease submit/release semantics remain characterized in `frame_prepare` tests: mark submitted only for a presented render outcome; texture lookup failure paths preserve typed error/cache clearing; RAII release remains owned by the lease.
- Architecture source test `state::tests::app_state_player_snapshot_boundary_stays_explicit` now checks begin-frame/publish order in the input snapshot owner and delegation-before-UI order in the coordinator.
- Verification for the change: `cargo test -p app-egui`; render-core/render-wgpu-video/render-wgpu-shell full tests; `cargo test -p player-core lease`; `cargo fmt --all`; refactor guardrails. Full `scripts/pre-pr-checks.sh` currently exits nonzero at dependency advisory policy because the lock graph contains newly reported RUSTSEC-2026-0194/0195 for transitive quick-xml 0.39.3; this is unrelated to frame decomposition and was not changed in Session 20.

## S42 executor wave 5 — private timing owner (2026-08-27)

- `crates/app-egui/src/frame_prepare/timing.rs` теперь владеет app CPU timing DTO, fallback frame budget, slow-frame threshold и подробной tracing projection. Модуль получает только immutable player snapshot и уже измеренные durations.
- `frame_prepare.rs` сохраняет acquisition/materializer/texture/cache lifecycle и orchestration; `submit.rs` по-прежнему единолично вызывает `mark_submitted_to_renderer()` только после реального `RenderFrameOutcome::Presented`.
- Texture Busy/Missing/Unsupported/Error outcomes, cache invalidation и lease release ownership не менялись. Parent/child production line counts: `1426/352`; новый child private и меньше 800 строк.
- Focused `frame_prepare::` suite: 33/33 PASS. Full app no-default и all-features: 1002/1002 в каждой matrix; strict Clippy обеих matrix, fmt/diff/refactor guardrails, S41 cross-provider integration 3/3, S42 final acceptance 24/24 и Serena diagnostics PASS.
