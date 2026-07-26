# S35S — neutral live same-item candidate switch (2026-07-24)

## Slice C superseding update (2026-07-26)

Live same-lineage restore now prepares position before authorization inside `PlayerSession`; app no longer forwards position for a new post-install seek. The staged candidate either carries a retained-DVR demux result into decoder landing or a typed safe-edge adjustment, then app adopts it with `InstalledPositionRestore::AdoptPreparedSameLineagePosition`. Current contract: `mem:player-core/staged-position-gate-slice-c-2026-07-26`.

## Ownership and transaction

- S35S extends the proven S25 same-lineage strong-open transaction; it does not add a provider-specific HLS/DASH switch path.
- Old playback continues through background preparation and until the existing authorization/commit boundary. After `EnqueuedAtPlayerOwner`, `CommitMustFinish` remains authoritative.
- Exact `Installed` still precedes `PlaylistRuntime::complete_same_item_candidate_switch`; this rebind preserves Item ID, app lineage, queue current, structural/traversal revisions, shuffle history/upcoming and visit history.
- App captures the old absolute live position and sends `InstalledPositionRestore::RestoreLiveSameItemPosition`. App does not inspect a DVR range, clamp a target or create a persistent live checkpoint.

## Player-owned live restore boundary

- `PlayerSession` owns the installed `DynamicMediaTimelinePort`, so it is the only layer allowed to decide the restore against the fresh new generation.
- Immediately after exact install/rebind, player observes the latest snapshot of the matching installed port and reapplies its newest revision before deciding.
- If the old absolute position is inside the fresh inclusive DVR range, the existing exact seek transaction runs and terminal success remains `InstalledMediaStateRestoreOutcome::Applied` only after the matching seek commit.
- If the target is outside the fresh range, or the new provider has no DVR, no seek is started. Player keeps/sets the provider-declared fresh safe `live_edge` and returns `InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge` with the requested position, chosen edge and typed `InstalledLiveEdgeAdjustmentReason`.
- Live success always maps to `InstalledCheckpointPosition::Live`; `ResumePositionWarning`, `NonSeekable` and durable resume persistence are not used.
- Old and new port generations never mix: replacement disconnects the old publisher, cancellation drops only the prepared new port/resource, and committed old/new demux resources follow exactly-once RAII release.

## Focused verification

- Neutral fake live-provider tests live in `crates/player-core/src/session/tests/live_same_item_restore.rs`: Playing/Paused, retained DVR, expiry during prepare/latest observation, no-DVR safe edge, stale generation isolation, committed exactly-once release and pre-barrier cancellation.
- App-focused routing tests live in `crates/app-egui/src/state/strong_media_open/pending/live_same_item_restore.rs`; existing S25/playlist tests continue to prove exact Installed/rebind and lineage/traversal invariants.
- Final checks: 592 `player-core` tests, 851 `app-egui --no-default-features` tests, strict Rust 1.96 all-target Clippy for touched crates, rustfmt, refactor guardrails, diff check and Serena diagnostics all pass.

## Known limitation

- If a retained DVR target expires after the fresh decision while the exact seek is already landing, existing S31L typed seek-expiry semantics apply. S35S does not add an automatic second transaction to jump to live edge.

Related: `mem:app-egui/same-item-candidate-switch-s25-2026-07-22`, `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`, `mem:app-egui/media-open-coordinator-s10c`, `mem:render-video/live-backend-swap-present-frame-freeze`, `mem:playlist/core`.