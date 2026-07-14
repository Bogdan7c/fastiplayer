# Playlist discovery

Session 09 completed PASS on 2026-07-14. This memory complements `mem:core`, `mem:playlist/core`, and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and dependencies
- `playlist-discovery` is the UI/player/config-neutral owner of single-local-file probe plus bounded deterministic non-recursive directory manifests.
- Normal dependencies are `media-core`, std-only `natural-sort-key`, `source-core`, `symphonia-demux`, and `thiserror`. It does not depend on `playlist-core`, app/UI/player/config/service crates, async runtimes, or concrete backends.
- The shared `natural-sort-key` crate owns only compact prepared natural comparison. Discovery owns native filename/path tie-breakers; playlist-core owns its locator/Item ID tie-breakers.

## Session 08 single-file probe
- Public `probe_one_local_media(&Path, &CancellationToken)` returns immutable `ProbedLocalMedia` or typed `ProbeOneLocalMediaError`.
- Success contains `LocalMediaKind::{VideoContaining, AudioOnly}`, optional `MediaDuration`, normalized `MediaTagMetadata`, lossy-safe display filename, and best-effort size + `SystemTime` mtime fingerprint.
- Errors remain distinct: `UnsupportedContainer`, `NoAudioVideoTracks`, `IoFailure`, malformed/other `ProbeFailure`, and `Cancelled`.
- `symphonia-demux::probe_open_local_media_file` owns Symphonia probing of an already opened `File`; extension is Hint only, topology includes null/unknown codec IDs, and no packet/decode loop runs. Explicit Play D64/D75 must use the later single prepared/open envelope instead of probe-first.

## Session 09 deterministic manifest boundary
- Public `build_directory_manifest(explicit_target)` creates an immutable `DirectoryManifest` before probe scheduling. It enumerates only the immediate parent; dot-hidden automatic siblings and directories are skipped, while an explicit hidden target is retained.
- Enumeration streams observations directly into `ManifestBuilder`; it never collects an unbounded `read_dir` vector. Skipped hidden/non-file/error entries still consume the raw entry count. Enumeration diagnostics retain at most 64 details plus one omitted-count summary.
- Records are natural ordered and contain only job-local `ManifestCandidateKey`, original locator, `NaturalPosition`, and bounded `ManifestAliasDiagnostics`. They contain no PlaylistItemId, probe result, player/app state, queue mutation, scheduler/admission state, or public canonical fallback.
- D63 membership is fixed after build. Later creates do not appear. `validate_candidate_source` checks one original locator without rescan and reports typed unknown-key, missing, source-changed, or unavailable diagnostics for delete/rename/symlink-retarget cases.
- Canonical-path dedup merges symlink aliases; hardlinks with different canonical paths remain distinct. D45 selection is explicit original path, otherwise direct non-symlink, otherwise deterministic natural + exact original alias. Canonicalization failure retains the absolute original locator with typed `io::ErrorKind`; making it absolute does not lexically collapse `..` after a symlink, so open semantics stay intact.
- Canonical identities are private transient dedup/validation state and are never returned as a hidden open fallback.

## Natural order and D73 bounds
- `natural-sort-key::PreparedNaturalKey` is std-only and holds one compact folded buffer (bytes, wide units, or Unicode u32 units), not an allocation-heavy token tree. Numeric runs compare without integer parsing; valid UTF-8 uses Unicode lowercase; non-UTF native/foreign units use exact ASCII fold.
- `RAW_MANIFEST_MAX_ENTRIES = 100_000`.
- `RAW_MANIFEST_MAX_PATH_KEY_BYTES = 64 * 1024 * 1024`; checked accounting includes retained original native paths, compact natural keys, and unique canonical identity paths.
- Entry, byte, or checked-arithmetic overflow returns typed `RawManifestLimitReached { limit, observed_at_least }`. The partial builder is dropped and no arbitrary read_dir prefix is returned.

## Verification and next scope
- PASS: 3 natural-sort-key, 60 playlist-core, and 24 playlist-discovery tests. Coverage includes comparator parity/total order, numeric/case/Unicode/non-UTF, shuffled input, hidden/non-recursive, create/delete/rename/retarget after snapshot, symlink/direct/alias-only/hardlink selection including mixed fallback/success aliases and preserved `symlink/../target` semantics, canonical fallback, exact 100k/100001 and 64 MiB/+1 limits, oversized path, checked overflow, no-prefix failure, and retained accounting. Main review also removed a duplicate retained canonical-path allocation so the counter matches actual builder storage.
- PASS: strict focused Clippy with `-D warnings`, fmt, Rust 1.96 and MSRV 1.92 locked workspace checks, guardrail unit tests/script, git diff check, and Serena diagnostics/references.
- Next allowed scope is Session 09A only: bounded executor/jobs/probe admission/readiness/result stream. App commit remains Session 15; executor/jobs were not started in Session 09.
