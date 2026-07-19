# Playlist current-media resume sidecar (2026-07-19)

## Ownership and boundaries
- `playlist-state::resume` owns the separate `playlist-resume.json` schema v1, strict bounded inspection, secret-safe exact locator fingerprint, explicit quarantine, atomic 0600 replace and latest-only writer lifecycle. Frequent position writes never serialize or touch `playlist-state.json`.
- `PlaylistRuntime::resume_persistence` owns capture policy, exact controller/player correlation, the live interval schedule, protected/non-persistent gating, retryable write reports and bounded shutdown. It accepts only confirmed `PlayerSnapshot`/exact seek/install facts; it does not own player, renderer or queue internals.
- `state/strong_media_open/pending/resume.rs` owns the startup post-Installed transaction. Public `player-core` API is unchanged: it uses exact `InstalledMediaStateRestore`/`PositionUnavailable`, then exact `StartPaused`, and only then registers active playlist lineage.

## Sidecar schema and safety
- Required fields: integer `schema_version = 1` and required nullable `checkpoint`. A checkpoint contains non-zero persisted `item_id`, lowercase 64-hex SHA-256 fingerprint of exact locator bytes, and `position { seconds, nanoseconds }` with nanoseconds `< 1_000_000_000`.
- Missing file and `checkpoint: null` are normal absence. Proven corrupt v1 is quarantined as `playlist-resume.corrupt-<timestamp>.json` without touching queue state. Newer/unrecognized/protected envelopes block writer start and are never overwritten by app policy.
- Fingerprints use a domain separator and exact URL/native/foreign path encodings. Raw locator/path/secret URL is absent from sidecar errors, reports and Debug output.
- Resume writer has a single newest pending slot, monotonic `ResumeSaveRevision`, duplicate/stationary suppression, retained terminal write report, and explicit bounded shutdown. Failed or durability-unconfirmed latest revisions become retryable; successful identical positions remain deduplicated.

## Runtime capture policy
- Checkpointing is enabled by existing `player.resume_last_position`. Live changes route through `player.apply`; disabling clears the in-memory startup candidate and stops future submissions, enabling starts the writer when lineage/store permit it.
- Only persistent lineage with writable inspected sidecar may write. Correlation requires exact current item, non-tombstone active media, player instance and `PlaylistBindingGeneration`. Stale instances/bindings, CLI/non-playlist active media and non-persistent/protected lineage are skipped.
- During `Playing`, periodic capture uses `playlist.resume_checkpoint_interval_ms` (default 5000, validated `1000..=60000`, step 1000). Live reschedule changes only the next deadline and preserves pending/latest snapshot.
- Immediate capture occurs after exact installed current change, exact seek receipt, transition to Paused/Stopped, Ended and terminal pre-player shutdown. Opening/Seeking/Scrubbing are never sampled by frame policy. Non-seekable media writes explicit null; Ended writes position zero.
- Queue persistence is flushed before resume writer join under the shared terminal deadline. Crash ordering is safe: queue-new/resume-old or resume-new/queue-old mismatches are ignored by item+fingerprint correlation.

## Startup semantics
- `StartupPosition::{KeepStart, Restore(Duration)}` makes position intent explicit. Only the original exact persisted current can consume the loaded sidecar checkpoint. Successful CLI open, ordinary opens, Next/Previous and Skip fallback targets use `KeepStart`/zero.
- Ordering is Installed -> exact position restore or recoverable fallback -> StartPaused -> active lineage registration. Successful restore has no message. A known position beyond current duration keeps the available start, overwrites the stale checkpoint and shows one nonfatal warning. `PositionUnavailable` for a now non-seekable source writes null and shows the same warning. Other seek/demux/receipt failures remain typed post-barrier failures and are not masked.

## Config and tests
- Config schema remains v6: both new `playlist.resume_checkpoint_interval_ms` and the now-active `player.resume_last_position` policy are backward-compatible/defaulted. `ConfigPaths::playlist_resume_file()` is the sole production path source.
- Focused coverage lives in `playlist-state/src/resume/tests.rs`, `app-egui/src/playlist_runtime/resume_persistence/tests.rs`, playlist settings/startup tests, and existing player exact-restore tests. It covers strict/null/corrupt/newer schema, permissions, latest-only/report retention, queue-file isolation, exact stale/tombstone/protected gating, periodic/immediate/Ended policy, non-seekable null, interval reschedule and exact startup fingerprint.
- PASS: focused suites; app all-features 692 tests; workspace tests/check on Rust 1.96; MSRV 1.92 check; strict all-features Clippy; strict rustdoc; fmt; refactor guardrails; diff check. `scripts/pre-pr-checks.sh` is externally blocked by new `quick-xml 0.39.3` advisories RUSTSEC-2026-0194/0195 plus unmaintained `audiopus_sys`; no dependency/baseline change was made. `scripts/coverage.sh check` reproduces the known workspace coverage ratchet defect; baseline was not updated.

Related memories: `mem:playlist/state`, `mem:app-egui/playlist-persistence-s14`, `mem:app-egui/startup-orchestration-s17`, `mem:playlist/settings-s13`, `mem:config/schema-store-decomposition-s23`.
