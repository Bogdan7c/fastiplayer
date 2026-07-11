# Session 21: timeline/live-scrub UI decomposition

`crates/app-egui/src/ui/timeline.rs` теперь является коротким compatibility facade и сохраняет прежние paths/re-exports для соседних app-egui модулей.

Owners:
- `ui/timeline/geometry.rs`: pure seekable bounds, fraction/time conversions, time formatting и visual rect geometry.
- `ui/timeline/gesture.rs`: `TimelineUiState`, normalized `TimelinePointerInput`, `TimelineAction` и deterministic gesture -> actions state machine без egui internals.
- `ui/timeline/live_scrub.rs`: pointer-down settings snapshot, throttle/latest-only, exact landing completion gate, stale landing rejection, stationary-target dedup, deferred-settings diagnostics и неизменный 250 ms fallback budget.
- `ui/timeline/render.rs`: egui `Response` adapter, focus-loss propagation, repaint decision, labels и painting. Renderer не владеет playback/decoder state.

Preserved invariants: pointer-down/click/drag/focus-loss sequencing; Begin -> Preview -> End/Cancel action order; live gesture route survives mid-drag setting changes; release uses exact pointer target; ThrottledLatest coalesces newest target and waits for exact landing or 250 ms fallback; EveryDragEvent attempts every distinct target; stale landing never opens reverse-drag gate; visual constants and layout unchanged. `AppState` remains the composition root that maps actions to `PlayerCommand` and receives presented landing events.

Focused tests live beside owners. Session checks: `cargo test -p app-egui`, app timeline command mapping tests, `cargo clippy -p app-egui --all-targets -- -D warnings`, `cargo fmt --all --check`, `scripts/check-refactor-guardrails.py`.