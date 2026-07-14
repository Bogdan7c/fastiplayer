# PlaylistController Session 11B

Session 11B completed PASS on 2026-07-14. This memory extends `mem:app-egui/playlist-controller-s11a`, `mem:playlist/core`, `mem:player-core/core`, and `mem:app-egui/media-open-coordinator-s10c`.

## Controller ownership and transport boundary
- Process-lifetime `PlaylistController` remains the sole app owner of manual transport policy. New cohesive implementation lives in `crates/app-egui/src/playlist_runtime/controller/transport.rs`; D08/D39 intent/token helpers are split under `controller/install/`. Mutable state does not move into renderer-bound `AppState`.
- Controller owns the latest explicit stable `Playing | Paused` intent and monotonic `PlaybackIntentRevision`, typed `Ui | Mpris` origin, app-owned `Active | Stopped` disposition, one latest D50 manual wait, stop-after-current latch, and one barrier/post-commit transport slot.
- Transient player snapshots do not rewrite stable intent. With a pending install, Play/Pause produces only the exact D52 `PlaybackIntentUpdate`; without a pending install it produces one exact-current transport request. No uncorrelated fallback or double Play/Pause dispatch exists.
- Play row uses stable Item ID: clean matching active instance restarts from zero without reopen (D59), terminal playback failure starts a normal reinstall, and matching nonterminal pending target coalesces into the existing request with `StartPlaying` (D60). Duplicate locators remain independent.
- Manual Next/Previous are ordinary one-step domain navigation. D17 restart-current runs first for `position > threshold`; zero disables restart and the typed threshold accepts only `0..=60_000 ms`. RepeatQueue alone wraps; RepeatOne still permits internal neighbours.
- D50 stores one cancellable wait without discovery wiring. Non-shuffle directions may wait, shuffle Next may wait, and shuffle Previous uses retained factual history only and returns immediate no-item when absent. Readiness re-evaluates the current queue and uses the latest stable intent.
- Manual install retains the domain-owned `PreparedManualNavigationToken` through exact Installed. Queue current, shuffle history/upcoming, dirty state, and allocator remain unchanged before commit.

## Guard, modes, stop-after-current, and neutral Stop
- D08/D39 guard keeps three phases. Before dispatch, Play/Next/Previous/Stop/stop-after-current exact-abort the reservation with their distinct cancellation cause. In `AuthorizationDispatchPending`, one latest race intent waits for authoritative cancel-win/enqueue-win; after enqueue/barrier one latest post-commit transport intent replaces older transport intents without FIFO.
- Lifecycle priority remains transport < suspend < shutdown. Terminal order is domain commit/abort, then one coalesced `DesiredQueueModes`, then one deferred transport/lifecycle intent.
- D58 clears a pending D50 wait or cancellation-carries `StopAfterCurrent`. Cancel-win applies the latest toggle to old current; enqueue-win applies it to new current after commit. Toggle-off only clears the latch and never resurrects a wait/request.
- Neutral Stop never uses destructive `PlayerCommand::Stop`. It targets one exact media instance through player-owned Pause then seek-to-zero. Only matching full success changes app disposition to `Stopped`; partial/failure/stale outcomes remain typed. Explicit Play clears Stopped. MPRIS-origin navigation from Stopped may install the next target paused while preserving app disposition.
- Cancellation/rejection outcomes retain `TransportStop`, `StopAfterCurrent`, `Superseded`, lifecycle cause, downstream rejection, and installed resolution separately.

## Player and domain additions
- `player-core::ExactMediaTransportRequest` supports `SetPlaybackIntent`, `RestartFromBeginning`, and `NeutralStop` with exact `MediaInstanceId`. `PlayerWorker::exact_media_transport` returns a request-owned receipt; bounded enqueue errors are distinct from owner `Applied | StaleInstance | Failed | PartiallyApplied` and missing-owner fatal outcome.
- Exact transport is serialized inside the player worker owner turn. Neutral Stop reports Pause failure separately from Pause-success/seek-failure; restart uses the existing replay-from-EOF boundary and never falls through to a newer instance.
- In shuffle mode, a reserved same-item reinstall records a factual visit and advances traversal revision even when Item ID is unchanged; structural revision and allocator remain unchanged.

## Verification and next scope
- PASS: 62 playlist-core tests, 522 player-core tests, 338 app-egui no-default tests, focused 13 controller transport tests and 6 exact player tests, strict Clippy for touched crates, `cargo fmt --all --check`, `cargo +1.96.0 check --workspace --locked`, refactor guardrails, `git diff --check`, and clean Serena diagnostics.
- Main review additionally removed a potential double Play/Pause dispatch by making D52 and exact-current delivery mutually exclusive.
- Explicitly outside 11B at completion: D53-D57 fast repeated concrete target preview/cursor (Session 11C), automatic Ended execution (Session 12), discovery wiring, config/schema, persistence/store, UI/hotkeys, and MPRIS backend wiring. Session 11C is now complete; continue with `mem:app-egui/playlist-controller-s11c`. Next allowed work is Session 12 only.
