# S42 core/DASH executor wave 4 (2026-08-27)

## Module boundaries

- `crates/config/src/validation.rs` remains the application-section router and generic validation owner (788 lines). `crates/config/src/validation/media_services.rs` owns audio/network/yt-dlp section validation plus the existing network/yt-dlp caps (194 lines). The established `crate::validation::*` constant and `validate_yt_dlp_config` paths are preserved by `pub(crate)` re-exports; setting names and `ConfigError::InvalidValue` payloads are unchanged.
- `crates/dash-mpd-core/src/parser.rs` remains the static MPD semantic/event orchestration owner (764 lines). `crates/dash-mpd-core/src/parser/attributes.rs` owns exact namespace/name checks, attribute allowlisting/lookup, bounded text/string allocation, and numeric/ratio decoding (174 lines). The parser-to-dynamic internal imports remain at the old `crate::parser::*` path through restricted `pub(super)` re-exports; XML budgets and typed errors are unchanged.
- `crates/web-media-dash/src/plan.rs` remains the presentation/component/period topology owner (562 lines). `crates/web-media-dash/src/plan/resources.rs` owns SegmentTemplate/SegmentList resource construction, BaseURL resolution through `HttpRequestTarget`, inclusive-to-bounded range conversion, serialized component construction, and fragment alignment (305 lines). Resource order, checked arithmetic, secret-safe target handling, query projection and typed `DashPlanError` outcomes are unchanged.
- No public cross-crate API, ownership/lifecycle contract, config/schema path, or test location changed.

## Verification

- Focused locked/all-feature suites passed: `rustiplayer-config` 93 tests; `dash-mpd-core` 17 integration tests; `web-media-dash` 48 unit/integration tests.
- Combined `cargo test -p rustiplayer-config -p dash-mpd-core -p web-media-dash --all-features --locked` passed.
- `cargo clippy -p rustiplayer-config -p dash-mpd-core -p web-media-dash --all-targets --all-features --locked -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`, and `python3 scripts/check-refactor-guardrails.py` passed.
- Serena diagnostics are empty for all six split files after project reactivation refreshed the new-module index.
- `python3 scripts/check_s42_guardrails.py` remains exit 1 for the known global inventory: coordinated legacy/stale baseline mismatches plus active new-production hard-limit violations in `crates/web-media-adaptive/src/fetch.rs` (1,135 > 800) and `crates/web-media-dash/src/live/runtime.rs` (1,080 > 800). The scoped Wave 4 delta appears solely as the expected stale reduction for `crates/config/src/validation.rs` (baseline 852, current 788); `scripts/module-size-baseline.json` was intentionally not edited. Both scoped DASH parents pass the hard line limit and do not appear in the violations.
