# H.264 frame cadence diagnostic boundary — session 01

Task: https://github.com/Bogdan7c/fastiplayer/issues/13, parent #10; branch `test/13-frame-quality`. Diagnostic production commit `ba63dc06bd4ba0cb244687ea631a678897b22b9f`. At this handoff push/draft PR were rejected by automatic approval review; do not infer publication or merge. Current exact status and artifacts: `user/cpu-optimization-plan/results/01.md`.

## Owners / workflow

`fastiplayer::frame_cadence=trace` adds three fixed-field, opt-in events without storing history or changing scheduler/release/API behavior. App `frame_prepare/submit.rs::submit_render_frame` reports the already prepared PTS + render/decode generations, acquisition and texture lookup reason and has_video_input. Shell `shell.rs::Renderer::render_frame` brackets surface acquisition including recovery/error and reports acquired=true/false. Existing `fastiplayer::video_render_acceptance=trace` is emitted only after Presented; `fastiplayer::render_frame_timing=trace` supplies durations. Detailed protocol: `docs/benchmarks/frame-cadence-diagnostics.md`.

Correlate the ordered events on the single render thread; UI-only frames and failed acquisitions are not video handoffs. Trace timestamps include observer overhead and are not GPU/scanout timestamps; surface wait is wall time, not CPU work. Diagnostic process lifetime/output must be bounded externally; there is no built-in rotation/time limit. Use isolated per-attempt XDG/transient service and preserve original snapshots per `mem:testing/local-cpu-measurement-2026-09-08`. Disabled trace has no retained history; no zero-cost or CPU-parity claim.

## Evidence / remaining gap

Fresh original H.264 observations preserved good/rare-repeat attempts and expected 30fps-on-60Hz repeats. Detailed current hardware traces distinguish two observed routes: (1) prepared previous frame with texture lookup Ready, newer scheduler selection during blocking surface acquisition; (2) texture-view Busy fallback. Decoded frames were ahead of selection and scheduler cadence remained near 16.67ms in the recorded windows. These facts do not establish that either mechanism explains the historical 1397/1800 deficit; no deterministic controlled-clock + delayed-acquisition rendering regression exists yet. Do not authorize session 02 or reorder the render loop solely from these traces. Original quality and CPU parity remain open.

## Validation locations

Existing `cargo test -p app-egui frame_prepare:: --locked` covers 33 preparation/lease cases; `cargo test -p render-wgpu-shell --locked` covers 19 renderer cases. Full `bash scripts/pre-pr-checks.sh` is required outside sandbox. Real diagnostic verification locally correlates every prepared identity through acquired surface to Presented handoff, confirms changing captured window output and quiet trace disablement. Private scripts/logs/snapshots are under `user/cpu-optimization-plan/artifacts/01/`; they are not public test fixtures or deterministic regressions. Exact latest checks and limitations belong in the handoff.
