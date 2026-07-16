# PlaylistController Session 12

Session 12 completed PASS on 2026-07-15. This memory extends `mem:app-egui/playlist-controller-s11c`, `mem:playlist/core`, and `mem:player-core/core`.

## Ownership and boundaries
- Process-lifetime `PlaylistController` owns automatic playback lifecycle in `crates/app-egui/src/playlist_runtime/controller/automatic_lifecycle.rs`: exact active Item ID + lineage + `MediaInstanceId` + player binding correlation, one observed terminal edge, D42 explicit hold, D26 deferred latch shell, error policy and all-failed summary.
- `playlist-core::queue::automatic` owns opaque fixed committed snapshot plans/tokens. App code sees target IDs and typed outcomes but never reads or writes shuffle history/upcoming. Exact Installed commits the D08 token and any generated RepeatQueue cycle atomically.
- `player-core` was not changed. Existing `PlaybackState::{Draining,Ended,Failed}`, snapshot `MediaInstanceId`, correlated events and exact `RestartFromBeginning` are sufficient neutral signals/boundaries.

## Exactly-once terminal lifecycle
- Matching Playing/Draining -> Ended or Failed is edge-triggered. Repeated terminal snapshots are no-ops; a matching non-terminal snapshot re-arms a later terminal edge for same-instance replay. Old binding/instance observations are stale.
- Explicit pending install, D50 wait and D53-D55 cursor create one D42 hold. Pre-concrete exhaustion/cancel may reevaluate once; concrete D55 failure stays awaiting-user. D56 Cancel and D57 structural invalidation consume matching Ended as distinct typed Stop outcomes without reevaluation.
- Automatic and dispatch-pending transitions retain a typed pending edge until cancel/enqueue winner. Cancel-win cannot retain a plan that a stale failure might resurrect. Enqueue-win commits B first; old snapshots cannot affect the new active identity. D58 enable consumes matching old Ended on cancel-win or latches the installed current after enqueue-win through the existing deferred transport order.
- Main review verified and fixed the pre-dispatch D58 edge for both an active D50 manual wait and an armed D26 deferred latch: enabling stop-after-current clears the wait/latch, consumes the exact matching Ended as a typed Stop, and removes released continuation state so repeated snapshots cannot remain silently held.
- Stop-after-current is a lineage latch and clears exactly when it consumes matching clean Ended. RepeatOne clean Ended uses exact replay; RepeatOne error always stops. Automatic OpenItem always uses StartPlaying.

## Error and traversal policy
- Stop/Skip are app-owned runtime policy values; settings wiring remains Session 13. Runtime/open badges stay D49 bounded/latest/correlated. Retry/skip preserve them; exact same-item Installed clears; source errors never remove or dirty rows (D70).
- A skip chain captures committed eligible Item IDs once and tracks a bounded attempted set. Late admission is excluded and does not invalidate Ready; removed members are revalidated and skipped without replacement. All-failed terminates with a typed attempted count.
- Shuffle automatic traversal uses domain-owned COW preview plus D08 token. Successful target consumes skipped path without fake factual visits and commits a newly generated RepeatQueue cycle without exposing storage internals.

## Verification and scope
- PASS: 363 app-egui no-default tests (54 controller; 14 focused automatic lifecycle), 64 playlist-core, 23 player EOF drain, 6 exact media transport, strict Clippy for app/core, fmt, Rust 1.96 locked workspace check, refactor guardrails, diff check, and clean Serena diagnostics for all touched production files.
- Focused coverage includes repeated terminal frames, replay re-arm, stale instance/binding, error-associated Ended/Failed, D42/D50/D55-D58, fixed late/removed snapshot, opaque shuffle cycle commit, D49/D70, stop latch and D26 cancel.
- At Session 12 completion, discovery execution, persistence/store/settings/UI, active tombstone/removal/Undo and Session 12A were not yet implemented.

## Session 15A discovery-navigation extension
- Session 15A completed PASS on 2026-07-16. `controller/discovery_navigation.rs` now owns the D41/D50 intent-level boundary without moving queue/frontier ownership into the controller: exact scope/active item/lineage/instance/binding matching, one `PendingManualTraversal`, `DiscoveryNavigationInterest`, and one-shot deferred automatic resume.
- Same-direction D50 presses retain the same wait identity and only reconfirm priority; opposite direction supersedes the logical frontier without FIFO work. Shuffle wait is allowed only for Next; Previous still follows retained factual history after D17 and returns the typed domain no-item outcome immediately when history is absent.
- Readiness snapshots the controller's latest stable D51 intent when `PlannedPlaylistInstall` is produced. Non-shuffle exact readiness must equal the domain's current manual/automatic target; shuffle admission only re-queries domain upcoming. A marker without a domain target leaves the same wait/latch armed and does not mutate traversal or dirty state.
- D58 cancellation clears the matching wait/latch/request action and traversal priority authority while the discovery bulk job remains alive. Cancel/fatal scan terminal clears matching continuation without reevaluation or resurrection; completed/limit terminal resolves matching wait as typed exhaustion.
- Successful readiness still returns only the existing planned manual/automatic install. D08 reservation/authorization/Installed remains the sole commit path, and the automatic D03 plan remains a fixed committed-ID snapshot that late admission cannot expand.
- Focused Session 15A coverage adds same/opposite coalescing, exact non-shuffle readiness once, responsive-shuffle admission/no-target retention, stale/cancel rejection, no pre-Installed queue/dirty mutation, and monotonic marker revision/direction checks. Next allowed playlist work is Session 16.


## Session 17 restore/startup continuation (2026-07-16)
- Controller получил intent-named restored-current boundary и bounded paused Stop/Skip chain; post-gate D65 actions удерживаются до authoritative cancel/enqueue winner, а queue/current/active identity коммитятся только через existing D08/Installed contract. Полный контракт: `mem:app-egui/startup-orchestration-s17`.
