# S31L — neutral dynamic live/DVR timeline (2026-07-23)

## Ownership and boundaries

- `media-core::dynamic_timeline` owns the provider-neutral contract. `dynamic_media_timeline(initial)` returns one move-only `DynamicMediaTimelinePort` and a source-side `DynamicMediaTimelinePublisher`.
- The immutable snapshot is fenced by `DynamicMediaTimelinePortGeneration`, monotonic `DynamicMediaTimelineEpoch`, and monotonic non-zero `DynamicMediaTimelineRevision`.
- Payload is stored as latest state under a mutex. A bounded capacity-one `crossbeam-channel` transports only a coalesced activity edge.
- The consumer protocol is observe → arm receiver → `recheck_after_arm`; do not apply a new revision silently while merely constructing the wait source. All applied changes must be followed by player output publication.
- `DynamicMediaTimelineState` has private fields and validated constructors: `without_dvr(live_edge)` and `with_dvr(live_edge, non_empty_range)`. A DVR end may not lie after the live edge.
- Stale source epochs and disconnected consumers are typed publisher errors. Old port/publisher pairs are isolated after media replacement.

## Prepared media and player session

- `PreparedMediaTimelineMode::{Static { playback_window }, Live { port }}` is the typed mutually-exclusive initial timeline intent. `with_playback_window` and `with_dynamic_timeline` return `PreparedMediaTimelineModeError` on CUE/live conflict; live additionally requires `duration=None`.
- `PlayerSession` owns the installed port, exact `MediaInstanceId`, observed revision, and disconnected flag. Replacement resets the binding.
- Public `TimelineSnapshot` has `TimelineMode::{Static,Live}`, `live_edge`, `live_epoch`, and `live_revision`. Live always has `duration=None`. No-DVR live is non-seekable with `TimelineNotSeekableReason::LiveWindowUnavailable`.
- Initial live install positions the logical cursor and media clock at the initial live edge. Later window slides update only the authoritative timeline/range, not the playback cursor.
- Live seek targets are never silently clamped. Targets outside the latest window fail with `PlayerErrorKind::SeekTargetExpired`; correlated exact seeks resolve `ExactTimelineSeekOutcome::Expired { requested_position, available_range }`.
- If the window moves during an active seek/scrub, player checks both the active commit target and the public latest scrub target; an expired route is cancelled and pending receipts resolve as expiry. A visible preview outside the latest DVR range is rejected with `OutsideLatestLiveRange` and falls back to a still-valid latest pointer target. Existing seek readiness, timeout, cancellation, `TemporarilyUnavailable` retry, and explicit terminal EOF machinery remain owners of their original lifecycle.

## Worker/app/desktop integration

- Both timed and indefinite worker waits include the optional dynamic activity receiver and perform the recheck immediately before blocking. `worker/runtime_wait.rs` owns select orchestration; `worker/runtime_timeline.rs` owns latest-snapshot application and disconnect handling. Disconnect disables that source and forces replanning to avoid a ready-disconnected busy loop.
- A successful revision publishes `PlayerSnapshot` even while paused, then invokes the payload-free `PlayerWorkerTimelineWake` bridge. App maps it to `AppWakeOwner::PlayerTimeline`, refreshes desktop state only for a changed `(media_instance_id, live_revision)`, and requests redraw.
- UI right label is `LIVE` without DVR and `DVR mm:ss–mm:ss · LIVE` with DVR.
- MPRIS receives no `mpris:length` for live media; `CanSeek` follows current DVR availability. Expired exact seeks do not emit `Seeked`.
- Live media never writes/restores a persistent resume position. Suspend/strong-open use typed KeepStart semantics for live instead of inventing a position.

## Focused tests

- `media-core/src/dynamic_timeline.rs`: validation, no-DVR/DVR, observe-arm-recheck, burst coalescing/latest snapshot, stale epoch/disconnect, old pair isolation.
- `player-core/src/session/tests/dynamic_timeline.rs`: public projection, sliding range without cursor movement, wait-source publication invariant, static/live conflict, typed expiry, replacement/disconnect.
- `player-core/src/worker/tests.rs`: paused idle worker wakes on a sliding live window.
- App/desktop tests cover live resume suppression, suspend KeepStart, expired MPRIS seek, capability/range and labels.

## S35S installed same-item restore extension (2026-07-24)

- `PlayerSession` now owns the fresh-generation DVR decision for `InstalledPositionRestore::RestoreLiveSameItemPosition`: it observes the latest snapshot of the exact installed port before deciding.
- A retained target uses the existing exact seek lifecycle. An expired/no-DVR target starts no seek, sets the fresh provider safe edge and returns typed `AdjustedToLiveEdge` with an exact reason.
- App cannot inspect/clamp the range and never persists a live checkpoint. Old/new generations remain isolated; cancellation and committed replacement have focused exactly-once release coverage.
- Expiry after a retained seek has already started continues to use the existing typed S31L seek-expiry outcome; there is no automatic second jump-to-edge transaction.
- Full handoff: `mem:app-egui/live-same-item-candidate-switch-s35s-2026-07-24`.

Related: `mem:player-core/core`, `mem:player-core/scrub-commit-policy-s09`, `mem:playlist/resume-position-sidecar-2026-07-19`, `mem:app-egui/wake-runtime-s10a`, `mem:app-egui/playlist-desktop-transport-s18b`, `mem:app-egui/live-same-item-candidate-switch-s35s-2026-07-24`.