# PlaylistController Session 12A

Session 12A completed PASS on 2026-07-15. This memory extends `mem:app-egui/playlist-controller-s12` and `mem:playlist/core`.

## Ownership and boundaries
- `playlist-core::queue::removal` owns the opaque `PlaylistRemovalSnapshot`, typed `RemovalCurrentOutcome`, exact canonical/current/shuffle/allocator restore-as-new-mutation, and stale revision guards. Removing committed current always persists `None`; no successor is assigned before exact Installed.
- `PlaylistItem` keeps locator + cached metadata in a private `Arc` payload. Capturing/restoring a 50k removal snapshot clones row handles but never deep-clones locator/metadata strings; shuffle state is already Arc-backed. `Arc::ptr_eq` focused coverage proves payload sharing.
- `PlaylistController` owns destructive Remove/Clear/RemoveOthers, D47 selection fallback, D57 structural invalidation, detached `ActiveMediaIdentity { item_id: None, .. }`, tombstone continuation, revalidation, reattach and release.
- `PlaylistRuntime` owns exactly one process-lifetime `RemovalUndoState`: typed kind, controller/domain snapshot, pre-removal selection, active lineage correlation and an 8-second `Instant` deadline. UI/store/discovery do not own or serialize this slot.

## Tombstone and continuation invariants
- Removing active/Clear never sends player Stop or reopen. The exact lineage, player instance and binding remain active while only the playlist row association is detached.
- Remove active captures an opaque fixed traversal context before mutation. Ended revalidates its target/members against the current committed queue. RepeatOne is normalized to StopAtEnd for tombstones, so the deleted row is never replayed. Clear stores no continuation at all.
- Failed or cancel-winner navigation, repeated Ended, and same-lineage D72 rebind preserve the tombstone. Only successful Installed of another lineage or process shutdown releases it.
- Successful continuation sets traversal current only through the existing exact Installed domain token. D57 matching already-Ended invalidation remains a typed Stop and contributes no second dirty revision.
- Same-lineage rebind requires the exact previously observed `ActiveMediaIdentity`; stale old instance/binding returns typed `Stale` and cannot overwrite a newer rebind.

## Undo invariants
- Deadline expiry is exact at `now >= deadline`; countdown uses monotonic time and exposes only the next visible-label/expiry wake.
- A second removal replaces the slot with the state immediately before that second mutation. No-op removal preserves the previous slot.
- Any later real persistent playlist mutation or successful new-lineage install invalidates Undo. Selection and other non-playlist actions, failed candidates, and same-lineage/new-instance rebind preserve it.
- Matching-lineage Undo restores queue/traversal/current/selection as a newer structural/dirty mutation and reattaches the current player instance without reopen. It never restores cancelled I/O or runtime error history.
- Pre-Ready installs are retired and cooperatively cancelled after a successful removal transaction. Ready/reserved/dispatch/in-flight phases return typed `InstallCommitLinearizing` and keep D08/D39 ownership intact.
- The obsolete Session 11 `remove_non_active` bypass was removed; all future destructive callers must enter through the process-runtime removal boundary.

## Verification and scope
- PASS: 69 playlist-core, 33 playlist-state, 387 app-egui no-default, 23 player EOF drain, 6 exact transport; strict touched-crate Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails, diff check, and clean Serena diagnostics.
- Focused coverage includes all removal kinds, D47, all repeat modes, fixed continuation revalidation, failed/cancelled retention, exact Installed release, deadline boundaries, second removal, metadata invalidation/no-op, 50k sharing, matching/stale lineage, same-lineage new instance, shutdown and new-lineage release.
- Session 13 settings ownership is complete. Persistence/load-gate/bootstrap/shutdown integration belongs to Session 14; UI/discovery remain later scope, and the real D72 suspend checkpoint/reopen flow belongs to Session 14B.