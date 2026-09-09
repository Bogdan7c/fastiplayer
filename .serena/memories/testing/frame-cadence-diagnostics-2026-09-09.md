# Frame cadence diagnosis — session 01

Task #13, tracking #10, branch `test/13-frame-quality`, PR https://github.com/Bogdan7c/fastiplayer/pull/14. PR #14 was merged by owner instruction on 2026-09-09 at 7dc707ce5bcef433791024ba260375fe80371aae. Session 02 issue #15 now has owner approval for the acquire-before-video-input architecture and its Git workflow; a new merge is not authorized. Exact head, check status, source archives and private artifact paths belong in `user/cpu-optimization-plan/results/01.md`.

## Current correction

Issue #15 moves app preparation after acquisition; current API/order and tests are in `mem:render-video/surface-acquire-before-video-input-2026-09-09`. The sections below record the prerequisite diagnostic version and its before-fix evidence; they must not be read as the current render order. Separate follow-up `mem:testing/frame-cadence-slot-diagnostics-2026-09-09` adds actual acquired/empty/busy slot outcomes: Ready texture alone never proved that the handoff slot was not Busy.

## Diagnostic owners

`fastiplayer::frame_cadence=trace` reports prepared PTS/render+decode generations and lookup/acquisition reasons in app `frame_prepare/submit.rs`, surface acquisition start/finish (including recovery/drop) in `render-wgpu-shell::Renderer`, and `latest video frame published` in player-core `LatestPresentFrameHandoff::publish`. The publication marker runs after handoff unlock and previous-lease drop, only for a nonempty published lease. Do not move it under the mutex: the manual causal test deliberately parks the publisher in this subscriber callback. This does not create a production blocking subscriber; normal logging has no barrier.

No scheduler/release/API/capacity changes. Correlate Prepared → acquire → actual Presented-gated `fastiplayer::video_render_acceptance` handoff, with independent worker publication events. Trace timestamps include observer overhead and are not GPU/scanout timing; surface wait is wall time, not CPU work. No retained diagnostic history or built-in log lifetime limit. Use bounded isolated attempts and preserve source/binary/config/media hashes per `mem:testing/local-cpu-measurement-2026-09-08`.

## Causal evidence and limits

Historical observations include 1397 distinct PTS/1800 handoffs: exactly 403 repeats plus 403 double-frame PTS steps across the window. Old logs have no acquire/Busy events, so do not attribute all those repeats to one path. Fresh detailed hardware traces separated 42 Ready previous-frame repeats (new selection after preparation, before acquire return) from four texture-view Busy fallbacks. Decoder drain was ahead of selection; generations and PTS/wall progression were stable in recorded windows.

Production-order causal repro now exists: three independent H.264 VA-API pairs on the actual AppShell/worker/swapchain. Publication BEFORE next preparation: 3/3 new PTS handoff PASS. Publication DURING acquisition after old frame preparation: 3/3 actual repeated PTS handoff and expected CADENCE_FRESHNESS_DEFECT. All targeted lookups Ready, generations equal, acquisition successful, ordinary process-owner shutdown completed. This proves the early-prepared input cause for the reproduced Ready path; it is not attribution of all historical 403 repeats or the Busy cause. Proposed next correction: take worker-selected input after successful acquisition, with API/ownership design reviewed separately. Session 02 implementation is tracked separately in issue #15; prerequisite PR #14 is merged. Do not infer completion from the approved design. No CPU parity, RAM parity or quality-fixed claim.

## Test locations and commands

`app_instance/cadence_acceptance.rs` and child `gate.rs` are Linux cfg(test) only, registered by app_instance/mod.rs. The wrapper delegates actual AppShell callbacks and normal shutdown. A global test subscriber holds worker after publication (outside locks), observes its first real handoff, then releases it either before next preparation (control) or at acquisition-start (regression), waiting for a newer published identity. Only interleaving is controlled; real clock/decode/renderer continue. Barriers are bounded at 3s, app deadline 25s; no production test hook/API. Read an existing CADENCE_CONFIG into a temp-directory-owned runtime; CADENCE_MEDIA is a local H.264 60fps fixture. Each ignored manual test MUST run alone in its own process because winit/global subscriber are process-wide:

`cargo test -p app-egui --locked -- --ignored --exact app_instance::cadence_acceptance::published_before_preparation_reaches_same_surface_handoff --nocapture`

`cargo test -p app-egui --locked -- --ignored --exact app_instance::cadence_acceptance::published_during_acquisition_reaches_same_surface_handoff --nocapture`

The second intentionally FAILS at freshness before a playback correction. Do not count environment, Busy/drop, barrier or startup failures as reproduced defects, and do not claim a green ordinary suite passes this ignored regression. Both use actual Presented-gated handoff, not pixel readback or physical scanout.

Separate `frame_prepare/shared_frame_materialization.rs` registers `cadence_tests.rs` + `cadence_gpu.rs`: two scripted HostPlanar publications through real materializer/PreparedVideoFrame/WGPU offscreen draw/readback, plus shared-lease accounting after GPU completion. It is a passing snapshot characterization, not the failing production-order regression. Its fake release sink does not qualify the actual decoder release bridge.

`bash scripts/pre-pr-checks.sh` is mandatory outside sandbox. Preserve manual expected failures separately. Detailed protocol is `docs/benchmarks/frame-cadence-diagnostics.md`; private scripts, all good/bad runs and executable/source snapshots are under `user/cpu-optimization-plan/artifacts/01/continuation/`.
