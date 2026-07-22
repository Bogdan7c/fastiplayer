# S25 — controlled same-item candidate switch (2026-07-22)

## Ownership and entrypoint
- URL sidebar publishes only a secret-safe `UrlSidebarAction::SelectCandidate { generation, candidate_index }`. Exact `YtDlpCandidateSelection` tokens remain private inside `WebMediaStreamConfiguration`; generation+index validation is the boundary.
- `AppState` owns the single-flight `PendingSameItemCandidateSwitch` and constructs a fresh `YtDlpCandidateOpenIntent::Exact`. The open path re-extracts in background and uses the existing semantic `rematch_exact`; old playback continues until authorization wins.
- Playback-window/group-part/CUE semantics are retained through `ActiveMediaSource::wrap_reopen_request`. Detached active media is valid: no queue current is invented and runtime item preference carries `item_id: None`.

## Commit protocol and lineage
- The shared stepwise strong-open envelope has an explicit `PendingStrongLineageCommit::{NewLineageOrQueue, SameLineage}` policy and `PendingStrongMediaAdmission::{Playlist, SameLineage}`.
- Same-lineage staging never creates/accepts a queue reservation. Immediately after exact `Installed`, before post-install restore can fail, it calls `PlaylistRuntime::complete_same_item_candidate_switch`, whose only controller mutation is `PlaylistController::rebind_active_media_same_lineage`.
- S25 must never call `register_external_strong_install`: that API creates a new app lineage. Queue current, structural/traversal revisions, shuffle history/upcoming/cursor, Item ID and lineage remain unchanged.
- Pre-barrier stale/failure performs lossless cancellation, clears the matching selector and preserves old playback. After `EnqueuedAtPlayerOwner`, the existing CommitMustFinish protocol applies. Suspend sends `LifecycleSuspended` losslessly and drains either cancel-win or install-win; shutdown relies on existing process-owner cancellation/drop authority.

## Fresh restore
- At `ReadyToCommit`, immediately before authorization, the app validates exact active instance/binding and captures fresh position, Playing/Paused intent, numeric volume and selected tracks.
- After exact `Installed`, video/audio IDs are restored only if the new A/V inventory contains the same ID+kind. Subtitle selection uses its separate owner contract: `None -> Disabled`, `Some(id) -> Select(id)`; subtitle tracks are not part of `TrackSummarySnapshot`.
- `InstalledMediaStateRestore` now contains explicit `InstalledVolumeRestore::{KeepCurrent, Set(f32)}`; invalid volume is reported as typed `InstalledMediaRestoreFailureStage::Volume`.
- Position uses the existing player playback-window mapping, so a window-relative target is converted at the player boundary. Fresh playback intent is acknowledged after state restore.

## Render lifecycle
- Exact same-item `Installed` starts the existing backend-swap present-frame freeze before candidate pointer commit. Old renderer resources remain Arc-owned until the new render generation yields a frame.
- If the new candidate is audio-only, generation switch plus absence of a video track terminates the freeze and releases the old frame; same/cross backend paths keep the existing exactly-once candidate release rules.
- See also `mem:render-video/live-backend-swap-present-frame-freeze` and `mem:render-video/controlled-renderer-recreation-s08c`.

## Verification
- Focused tests cover fresh Playing/Paused capture, exact/disabled subtitle restore, applicable A/V tracks, exact volume and typed invalid-volume failure, playback-window relative seek, detached active source, compound part current, selector busy/stale/failure state, queue/traversal/shuffle/lineage preservation, and existing same/cross-backend/cancel/release scenarios.
- Final verification: 568 `player-core` tests PASS; 825 `app-egui --no-default-features` tests PASS; strict all-targets Clippy for both touched crates, default-feature app check, rustfmt, refactor guardrails, diff check and Serena diagnostics PASS.
- Related memories: `mem:playlist/core`, `mem:app-egui/media-open-coordinator-s10c`, `mem:render-video/live-backend-swap-present-frame-freeze`.
