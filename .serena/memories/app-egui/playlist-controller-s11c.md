# PlaylistController Session 11C

Session 11C completed PASS on 2026-07-14. This memory extends `mem:app-egui/playlist-controller-s11b`, `mem:playlist/core`, and `mem:app-egui/media-open-coordinator-s10c`.

## Ownership and cursor boundary
- Process-lifetime `PlaylistController` now owns one `ManualNavigationCursor` in `crates/app-egui/src/playlist_runtime/controller/manual_navigation.rs`. It is the only app-side owner of the runtime logical cursor; `playlist-core` remains the exclusive owner of committed traversal plus opaque D54 preview/token semantics.
- A→B→C before authorization is latest-only. No FIFO exists, intermediate rows never become queue current/active and no dirty revision is published. The cursor stores one preview plus a bounded two-request stale tombstone window: current terminal and the coordinator's maximum one non-cancellable stale preparation.
- Generic `MediaOpenCoordinator` remains policy-neutral. Queue target, cursor, repeat/shuffle, failure policy and Ended shell do not cross into coordinator.

## Ready/barrier/commit ordering
- Matching Ready transfers exactly one cursor preview into `PreparedManualNavigationToken`. Pre-dispatch supersede/backtrack/Cancel exact-aborts the token, recovers the same opaque preview, then continues or discards it.
- `AuthorizationDispatchPending` never speculatively aborts. Cancel-win restores the exact preview before one deferred cursor intent; enqueue-win preserves the token, exact Installed commits it infallibly, and only then one post-commit intent runs relative to the new active lineage.
- Exact manual Installed publishes one traversal/dirty commit. D54 shuffle intermediate IDs become consumed but never factual history. The 50k controller test proves fast cursor updates reuse shared structural rows and do not dirty/commit before Installed.

## D55–D57 manual chain
- Concrete manual target failures, including downstream rejection before player enqueue after Ready, enter typed `AwaitingUserAfterFailure`. Retry repeats the exact target; its boundary distinguishes `StartInstall`, `InstallAlreadyInProgress`, and `NoFailedTarget` instead of reporting a false empty queue. Next/Previous continue the same preview; explicit Cancel discards it. There is no D03 skip or automatic reevaluation.
- Pre-concrete D50 probe rejection is a distinct `ContinueWaiting` outcome and cannot create D55 state. Stable Play/Pause confirmation changes preserve the cursor and remain Session 14A confirmation semantics.
- Session 11C adds only a typed Ended-origin shell, not Ended detection: future Session 12 may mark an exact matching origin Ended. Explicit Cancel then returns `StopEndedOrigin`; active-origin Cancel returns `KeepActive` and does not arm a future stop.
- Structural mutation returns a distinct `StructuralInvalidation`, discards preview, preserves the matching Ended terminal action, rejects stale results, and advances only the real mutation dirty revision.

## Layout and verification
- Owner-focused production sizes after self-review: `controller/install.rs` 766, `controller/transport.rs` 793, `controller/manual_navigation.rs` 492. Manual cursor tests are separate under `controller/manual_navigation/tests.rs`; install-specific cursor helpers are in `controller/install/manual.rs`.
- PASS: 40 focused controller tests, 62 playlist-core tests, full 349 app-egui no-default tests, strict app/core Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails, diff check, and Serena diagnostics.
- Explicitly outside 11C: automatic Ended policy/execution, discovery/store/config/UI/hotkey/MPRIS wiring, tombstones and Session 12 work. Next allowed session is Session 12 only.
