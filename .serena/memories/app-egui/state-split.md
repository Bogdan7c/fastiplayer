# app-egui state split

S14 refactor-only split moved `crates/app-egui/src/state.rs` implementation details into private child modules under `crates/app-egui/src/state/` while preserving `AppState` as the owner and preserving existing external paths through re-exports where needed.

- `state.rs`: keeps `AppState` fields/constructor, frame context/timing/output structs, committed snapshot/player snapshot/redraw/desktop integration core helpers, and declares private child modules.
- `state/present_frame_cache.rs`: `PresentFrameAcquisition`, `RenderablePresentFrame`, cached/renderable present-frame identity, validation, lifecycle invalidation, texture-busy fallback, render error/frame cache methods.
- `state/video_backend.rs`: video pipeline init/rebuild, backend reselection, backend swap freeze/phase, system capabilities handoff.
- `state/media_jobs.rs`: `ActiveMediaSource`, local file/prepared media/direct/YouTube loading, local open job polling/result application, media reconfigure restore helpers.
- `state/telemetry_panel.rs`: telemetry panel cache/rows/tone, row building and panel rendering helpers.
- `state/ui_runtime.rs`: `render_ui`, control actions, timeline action mapping, fullscreen/hotkey/open-file UI glue, center overlay, frame counters snapshot.
- `state/tests.rs`: former state tests; guard tests build a combined source string from `state.rs` and the child modules so existing architecture assertions still cover moved code.

Visibility policy for this split: no new `pub(crate)`/`pub` boundary beyond preserving existing public/internal API paths; cross-child helpers use only `pub(super)` inside the private `state` parent module. Behavior, config, diagnostics, UI layout/sidebar viewport behavior, backend selection, cached-frame lifecycle, and test assertions were intended to remain unchanged.

Focused checks used for this split: `cargo test -p app-egui state`, `cargo test -p app-egui`, `cargo fmt --all --check`.