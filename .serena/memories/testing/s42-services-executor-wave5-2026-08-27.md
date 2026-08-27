# S42 services executor wave 5 (2026-08-27)

## Module boundaries

- `crates/web-media-adaptive/src/fetch.rs` remains the owner of public `AdaptiveHttpContext`, the shared `HttpSourceSession`, source generation/cancellation, endpoint-expiry observation, public request/result/error types and completed-resource cache integration. It is 720 lines after formatting (previously 1,135).
- `crates/web-media-adaptive/src/fetch/execution.rs` is the private physical execution owner (441 lines): `FetchPurpose`, jobs/outcomes, bounded blocking worker pool, buffered and streaming manual redirect traversal, request material, and cancellation-aware retry waits.
- Existing crate-internal `crate::fetch::{FetchExecutor, FetchJob, FetchOutcome, FetchPurpose, FetchSuccess, wait_for_any_cancellation}` paths remain available through restricted `pub(crate)` re-exports. No public cross-crate API changed.
- Redirect authorization, monotonic secret suppression, same scoped query/header projection, exact status/expiry reporting, bounded body/range semantics, retry-after policy and interruptible streaming body ownership are unchanged.
- `crates/web-media-dash/src/live/runtime.rs` remains the owner of shared authoritative snapshot/http/generation/revision state, endpoint recovery serialization, `DashLiveDemuxer`, track publication, refresh/replacement lifecycle and public open request/result/error API. It is 654 lines after formatting (previously 1,080).
- `crates/web-media-dash/src/live/runtime/open.rs` is the private open owner (455 lines): initial manifest prepare, selected component/transactional A/V assembly, continuation assembly, selection lane facts and endpoint resource remap.
- Initial DASH open still clones the plan under a short state guard, releases the guard before re-entrant component open, then re-reads the authoritative live edge after open because synchronous endpoint recovery can replace the snapshot. Endpoint remap still uses media kind + Period timeline identity + resource kind/range/timeline/duration, deliberately never the old/fresh secret URL.

## Verification

- Focused adaptive tests passed for stalled streaming seek cancellation, physical source cancellation and monotonic cross-origin header/query-secret stripping.
- Focused DASH identity-remap test passed from its new private owner.
- `cargo test -p web-media-adaptive -p web-media-dash --all-features --locked` passed: adaptive 61/61; DASH 37 unit, 4 dynamic-runtime, 3 live-runtime and 4 representation-catalog tests.
- `cargo clippy -p web-media-adaptive -p web-media-dash --all-targets --all-features --locked -- -D warnings`, `git diff --check` and `python3 scripts/check-refactor-guardrails.py` passed.
- Serena diagnostics are empty for both parent and both new private modules after project reactivation.
- `python3 scripts/check_s42_guardrails.py` no longer reports either former active-new violation. It still exits 1 for the pre-existing/parallel global baseline inventory; the size baseline was intentionally not edited.
- The first global `cargo fmt --all -- --check` observed a parallel agent's transient unformatted import block; after that owner finished, the repeated global fmt check passed without this task touching the foreign file.

Related: `mem:media-services/adaptive-transport-s31-2026-07-23`, `mem:media-services/dash-live-s35-2026-07-24`, `mem:media-services/http-retry-after-aud017-2026-08-24`, `mem:media-services/manifest-supersede-cancellation-aud020-2026-08-24`, `mem:testing/s42-core-dash-executor-wave4-2026-08-27`.