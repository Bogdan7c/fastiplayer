# S01G — Compound-core hardening gate (2026-07-20)

## Verdict
- S01G PASS for S01P/S01Q and S01A–S01D. No new feature logic, production Rust, public/internal API, dependency or owner-boundary changes were required.
- Serena symbol/reference audit covered every public/internal `PlaylistQueue` mutation family: single/entry append and replace, capped append, discovery insertion, direct/prepared sort, single/multi move, single/bulk remove, Clear, removal snapshot/Undo restore, metadata preflight/commit, traversal current, reserved replacement/select, manual and automatic traversal prepare/abort/commit, repeat/shuffle toggle/direct play, queue restore and shuffle restore.
- External production mutation authority remains only in `app-egui::PlaylistController`/runtime owner turns. `playlist-discovery` remains queue/ID/policy-neutral. No unaccounted mutator escaped to app/session code.

## Proven invariants
- Capacity and allocator accounting count retained parts; one-part compound stays compound; empty compound is typed rejected; capped prefix never splits a compound.
- Structural operations address `PlaylistEntryId`; subordinate part targets are typed rejected. Discovery anchors are top-level; sort/move/remove/Undo preserve internal part order and exact Group/Item IDs.
- Manual/automatic/repeat/shuffle/reservation traverse exact playable parts while shuffle upcoming remains top-level entry identity. Ready/reservation does not publish current/history; exact commit does.
- Item and Group allocator high-watermarks survive `Clear`, compound `replace_entries`, and the legacy single-only strong-install reserved replacement without regression or accidental Group-ID burn.
- Borrowed compound traversal is derived directly from nested entries and remains `ExactSizeIterator + DoubleEndedIterator`; alternating `next`/`next_back` keeps exact remaining length. `PlaylistQueue` owns exactly one direct `Vec`, `Vec<PlaylistEntry>`, and no direct `Arc`/owned playable snapshot field.
- Legacy `PlaylistQueue::items()` and ambiguous queue `len()` callers remain absent. `OwnedPlayableItemsSnapshot` exists only for explicit ownership handoff, never as mutation authority or queue cache.

## New focused evidence
- `crates/playlist-core/src/queue/entries/tests.rs`: `item_and_group_high_watermarks_survive_clear_and_both_replacement_paths`.
- `crates/playlist-core/src/queue/read/tests.rs`: `derived_compound_iterator_keeps_exact_len_when_both_ends_are_consumed` and `playlist_queue_storage_and_owner_modules_stay_hardened`.
- Module-size guardrail automatically scans every production `queue` Rust module: default maximum 800 lines; tighter named limits for `queue/mod.rs` (700), `queue/entries.rs` (600), `queue/shuffle/runtime.rs` (750); explicit existing vocabulary exception `queue/outcomes.rs` (900). `entry.rs` and `payload.rs` are limited to 700. New test modules are excluded by filename, not by a manually maintained production-module list.

## Verification
- `cargo test -p playlist-core --all-features`: 122 PASS.
- `cargo test -p playlist-state --all-features`: 41 PASS.
- `cargo test -p app-egui --all-features`: 722 PASS.
- strict `cargo clippy -p playlist-core --all-targets --all-features -- -D warnings`: PASS.
- `cargo check --workspace --all-features --locked`: PASS.
- `cargo +1.92.0 check -p playlist-core --all-targets --all-features --locked`: PASS.
- `cargo fmt --all -- --check`, `git diff --check`, Serena diagnostics and `scripts/check-refactor-guardrails.py`: PASS.
- `cargo deny check`: expected FAIL only for existing transitive `quick-xml 0.39.3` RUSTSEC-2026-0194/0195; bans/licenses/sources pass. S01G changed no dependencies and does not own the S04X advisory fix.

## Remaining scope
- `playlist-state` schema v2 and compound restore/persistence remain S02.
- Import-draft-to-queue transaction remains S08; compound runtime/UI/MPRIS projection remains S17G/S17V/S17M.
- S01G must not be used as permission to add a bool bypass, flatten a compound, cache a parallel playable Vec, or move mutation authority out of `PlaylistQueue`.
