# S42 player session root decomposition (2026-08-27)

Behavior-neutral split of the central `PlayerSession` implementation. Public API paths, signatures, typed command/error outcomes, state layout and test locations are unchanged.

## Module ownership

- `crates/player-core/src/session.rs` is 782 lines. It remains the owner of the complete private `PlayerSession` state layout, constructors, `Default`, snapshot/query boundaries, high-level `dispatch_command`, event drains, render/frame lifecycle bridges and video-backend installation boundary.
- Private child `crates/player-core/src/session/position_clock.rs` is 325 lines. It owns absolute source ↔ public relative position mapping, audio-authoritative presentation time, no-audio monotonic anchor lifecycle, scheduler deadline projection, seek-target resolution and playback-window packet/frame admission.
- Private child `crates/player-core/src/session/runtime_control.rs` is 354 lines. It owns Play/autoplay/Pause/Stop, volume/mute plus last-nonzero restoration, track/quality/config commands, idempotent shutdown, playback/error/position state publication.
- Relocated helpers used across the `session` subtree use `pub(super)`, preserving the former parent-private effective visibility without widening crate/public API. Helpers referenced only inside their new child deliberately remain private for least privilege. Existing `pub` and `pub(crate)` methods retain their visibility and continue to be accessed through `PlayerSession`, not through child module paths.

## Preserved invariants

- Audio clock remains authoritative whenever installed; no-audio playback uses the existing playback-rate-scaled monotonic anchor and freezes/reanchors on Pause/Play without a wall-time jump.
- Position events still publish after the clock sample and no-audio reanchor; playback-state events still follow the state/EOF-drain mutation.
- Volume zero preserves `last_nonzero_volume`; unmute restores remembered, valid fallback, then 1.0 in that order.
- Video track selection still crosses `select_requested_video_track`, while audio selection crosses the pipeline-owned track boundary.
- Shutdown remains accepted repeatedly and closes the same staged-install/demux-retry lifecycle. Post-shutdown commands that cross `ensure_not_shutdown` remain typed `InvalidCommand`; `SetPlaybackRate` deliberately does not cross that gate and preserves its pre-existing state-dependent `Applied`/`Rejected` behavior.
- Fatal/recoverable errors, backend reselection, render release and seek receipt semantics were not moved or changed.

## Functional verification

- Focused production-boundary tests passed: session playback 16/16, playback rate 13/13, playback window 11/11, capability selection 29/29, exact media transport 8/8, cold resume A/V 1/1, scheduler presentation 13/13.
- Full `cargo test --locked --all-features -p player-core`: 669/669 plus doc tests.
- Strict `cargo clippy --locked --all-features --all-targets -p player-core -- -D warnings`: passed.
- `cargo +1.96.0 test -p service-ytdlp --test final_acceptance_s42 --locked`: 24/24.
- `python3 scripts/check_s42_guardrails.py` intentionally reports the coordinated stale module-size snapshot, including `session.rs` baseline 1392 → current 782. Per executor scope, `scripts/module-size-baseline.json` was not changed; the same aggregate report also includes unrelated concurrent module decompositions.
- Owned rustfmt, repo-wide fmt check, `git diff --check`, and `scripts/check-refactor-guardrails.py`: passed.
- Reviewer reproduced a pre-existing process-global tracing callsite-interest race: thread-local capture could miss production `info!` markers when a parallel seek test touched the same callsites. The two trace-sensitive A/V tests now trampoline through exact `current_exe` libtest children with a child-only recursion marker, `--test-threads=1` and a captured execution marker proving the exact test was selected; child failure preserves exit status, stdout and stderr. The parallel three-test A/V group passed 100/100 repetitions, both exact child bodies passed 1/1, and the complete 669-test suite passed 50/50 repetitions. Production behavior and trace callsites remain unchanged.

Related: `mem:player-core/core`, `mem:player-core/audio-runtime`, `mem:player-core/playback-rate-contract-s32`, `mem:testing/s42-playback-test-layout-2026-08-27`.
