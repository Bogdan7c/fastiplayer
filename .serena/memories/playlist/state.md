# Playlist state

Session 06 completed PASS on 2026-07-14. This memory complements `mem:core`, `mem:playlist/core`, and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and dependencies
- New workspace crate `playlist-state` owns disk schema v1, JSON serialization/deserialization, bounded inspection, inspected-source identity, and explicit quarantine.
- Dependency direction is `playlist-state -> playlist-core` plus direct `media-core` access required by public neutral metadata value types. Normal implementation dependencies are `playlist-core`, `media-core`, `serde`, `serde_json`, `sha2`, and `libc`.
- `playlist-core` remains serde- and I/O-neutral with only `media-core` and `rand` normal dependencies.
- Caller supplies the exact state path/config directory. This crate does not read AppConfig/ConfigPaths, own the D10e instance lock, integrate app/UI/player/demux, or implement a save worker.

## Public boundary and schema
- Filename is `playlist-state.json`; current required top-level `schema_version` is integer 1.
- Serialization accepts immutable `PlaylistStateSnapshot { queue: &PlaylistQueue, repeat_mode }`; load yields `LoadedPlaylistState { queue, repeat_mode }`.
- Typed inspection outcomes are Missing, Loaded, CorruptNeedsQuarantine, NewerSchemaSaveBlocked, and UnrecognizedVersionSaveBlocked.
- Typed quarantine outcomes are Applied, SourceChanged, and FailedSaveBlocked. Inspection is read-only; quarantine happens only through the explicit app-policy call.
- Private v1 DTO uses explicit DTO/domain mapping and deny-unknown-fields. It persists exact canonical items, required nullable current, repeat, shuffle/history/cursor/upcoming, retained factual history tail, required `next_item_id`, full D12 display/sort cache and fingerprint.
- Domain restore remains the owner of queue invariants: capacity 50,000, unique non-zero IDs, required watermark strictly greater than max ID, references, cursor, repeated factual history, and duplicate-free exact upcoming. There is no max+1 allocator fallback and valid current None is not repaired to the first row.

## Locator and secret safety
- Raw direct URLs are read only through `SecretUrlLocator::expose_secret_for_persistence`. Public errors/outcomes and Debug snapshots do not format secret locators automatically.
- A normalized YouTube locator is persisted as the already-provided domain URL identity; there is no service dependency.
- Every local DTO carries exact origin platform (Linux, MacOs, Windows, Other) even for UTF-8 and exact encoding (UTF-8, bytes, wide, opaque units).
- Only matching native platform/encoding constructs PathBuf. Foreign or unsupported platform locators stay opaque and serialize again without lossy conversion.

## Bounded inspection
- A streaming envelope pass precedes supported DTO parse and scans the entire top-level object.
- Hard envelope limits are 40 MiB total, 1 MiB decoded token, and depth 128. Exactly one integer top-level schema_version is required; nested spoof is ignored.
- Duplicate/conflicting version keys, malformed/missing/non-integer version, or budget exhaustion before full uniqueness proof produce protected no-touch save-block.
- Newer schema is protected before applying current-v1 payload limits.
- Only after schema v1 is proven does the 32 MiB supported-v1 file limit and per-field/collection validation apply. An oversized proven v1 inside the envelope budget may be classified corrupt.

## Identity and quarantine
- Inspection uses no-follow regular-file classification. Identity contains platform file ID when available (Unix device+inode), length, mtime, and SHA-256 digest of inspected bytes.
- `apply_quarantine` reopens no-follow, revalidates metadata and digest, and then renames to `playlist-state.corrupt-<timestamp>.json` without collision overwrite.
- Linux uses `renameat2(RENAME_NOREPLACE)`; the fallback preserves no-overwrite with link/unlink semantics.
- There is no portable atomic compare-identity-and-rename. In-process writer/quarantine operations must share one `PlaylistStateStore` mutex, but an external TOCTOU window remains after revalidation and is explicitly documented.
- Deterministic naming is injected through `QuarantineFileName::from_timestamp`.

## Verification and next scope
- 15 playlist-state tests plus 60 playlist-core tests pass. Coverage includes full cache/URL/path variants, persisted allocator high watermark across remove/Clear/load, strict version envelope cases, all supported corrupt invariants, no-follow inspection, explicit quarantine collision/source-change/rename-failure, and newer-schema no-touch.
- Strict focused Clippy, fmt, Rust 1.96 and MSRV 1.92 locked workspace checks, guardrails, and git diff checks pass.
- Dependency audit still has only the known D28 blocking quick-xml advisories; the known audiopus_sys advisory remains non-blocking.
- Serena rust-analyzer diagnostics for `dto.rs` were stale after the module split (reporting removed symbols), so fresh Cargo checks/tests/Clippy are the authoritative verification for this session.
- Next allowed work is Session 07 only: latest-only atomic save worker and durability lifecycle. Permissions, atomic save, and background worker were intentionally not started in Session 06.
