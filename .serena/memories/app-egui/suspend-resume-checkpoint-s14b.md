# Session 14B — process-lifetime active media suspend/resume checkpoint

Session 14B completed PASS on 2026-07-15. This memory extends `mem:app-egui/playlist-persistence-s14`, `mem:app-egui/playlist-controller-s12a`, `mem:app-egui/media-open-coordinator-s10c`, and `mem:player-core/core`.

## Ownership

- `PlaylistRuntime` owns the runtime-only `SuspendedMediaCheckpoint`, reconstructible active source, resume status, typed warning/error and explicit Retry/supersede policy. Nothing from this checkpoint is serialized into `playlist-state`.
- `AppState` owns only one renderer/player-binding resume executor: detached video candidate resources and request-owned preparation/install/seek/intent/release receipts. Dropping or recreating `AppState` never drops checkpoint authority.
- `PlaylistController` remains the owner of `ActiveMediaIdentity`, app lineage, stable Playing/Paused intent, stop-after-current, tombstone/Undo correlation and the consumed Ended edge.
- `MediaOpenCoordinator` remains the owner of preparation, Ready/authorization barrier and exact terminal delivery. `player-core` remains the owner of installed media, seekability and exact candidate release.
- `AppShell` owns ordering only: resolve/cancel pending media, capture or preserve the existing checkpoint, detach the old binding, create a new binding, and drive the non-blocking resume executor through existing wake/poll paths.

## Suspend invariants

- Capture happens before player binding detach and records the reconstructible local/direct/exact-selected-YouTube source, optional item/tombstone inside the active identity, app lineage, confirmed position, stable intent, and old instance/binding only for stale rejection.
- Pre-dispatch work is terminal-cancelled. `AuthorizationDispatchPending` waits for authoritative cancel/enqueue resolution without speculative token abort. Enqueue winner drains exact Installed/controller commit/deferred modes before checkpointing the winning new lineage.
- Missing resolution/terminal, fatal disconnect and stale identity are typed lifecycle failures, never a successful empty checkpoint.
- A repeated suspend during Ready, terminal-failed, recoverable-failed or in-progress resume preserves and re-arms the existing checkpoint instead of comparing the released fresh-binding player against the old instance.
- Suspend does not flush/reset queue, sibling owners, Undo, stop latch, errors, dirty state, persistence worker or replacement confirmation.

## Resume transaction

- The only valid order is `StartPaused install -> exact Installed -> exact seek or typed PositionUnavailable -> stable Playing/Paused intent -> same-lineage rebind`.
- No active controller identity is published for the new instance until seek/non-seekable resolution and final intent receipt succeed. Old instance/binding events cannot commit or consume a new edge.
- Ended is stored as Paused-at-end and carries an already handled EOF edge to the rebound instance. Failed runtime state requires explicit Retry; recoverable prepare/install/seek/intent failures also keep the checkpoint for explicit Retry and never run playlist error-policy auto-skip.
- Non-seekable live media keeps the nearest newly available position and publishes bounded `ResumePositionWarning`; it never mutates the queue or hides a navigation.
- Post-install failure uses the exact request+`MediaInstanceId` player release boundary. Pre-Installed failure consumes/cancels the coordinator terminal so Retry cannot see a hidden Busy slot.
- A genuinely new explicit strong open first terminal-resolves any in-progress resume with typed Superseded cause, then clears the checkpoint. Every successful external strong install registers a new controller lineage together with its actual stable install intent; playlist-reserved installs retain their existing commit path.
- Exact selected YouTube identity is reopened through the service-owned selected-stream resolver, preserving live/non-seekable fallback without choosing a different stream silently.

## Player-core boundaries

- `InstalledMediaRelease` and its request-owned receipt target exact request+instance. Outcomes are Applied, Absent, StaleInstance, Failed, and MissingOwnerOutcome at receipt level. Validation and release occur in one serialized player-owner turn, so a late cleanup cannot affect newer media.
- `InstalledMediaStateRestoreOutcome::PositionUnavailable` carries requested and available positions plus typed `InstalledPositionUnavailableReason::SourceNotSeekable`. Other seek failures remain failures; error strings are never parsed.

## Verification

- PASS after main review: 467 `app-egui --no-default-features` tests, 530 `player-core --lib` tests, 33 `service-youtube` tests with 4 manual ignores, focused checkpoint/exact-restore tests, strict all-target/all-feature Clippy for touched crates, `cargo fmt --all --check`, Rust 1.96 locked workspace check, refactor guardrails and `git diff --check`.
- Serena diagnostics are clean after refresh; an initial stale rust-analyzer signature diagnostic disappeared on re-query.