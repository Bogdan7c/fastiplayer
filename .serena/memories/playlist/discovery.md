# Playlist discovery

Session 08 completed PASS on 2026-07-14. This memory complements `mem:core`, `mem:playlist/core`, `mem:symphonia-demux/core`, and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and public boundary
- `playlist-discovery` is the UI/player/config-neutral owner of single-local-file discovery intent. Public `probe_one_local_media(&Path, &source_core::CancellationToken)` returns immutable `ProbedLocalMedia` or typed `ProbeOneLocalMediaError`.
- A successful record contains `LocalMediaKind::{VideoContaining, AudioOnly}`, optional neutral `media_core::MediaDuration`, normalized `media_core::MediaTagMetadata`, lossy-safe display filename, and `LocalMediaFingerprint` with exact opened-handle size plus `SystemTime` mtime. The fingerprint is best-effort cache invalidation, not a content hash.
- Error taxonomy stays distinct: `UnsupportedContainer`, `NoAudioVideoTracks`, `IoFailure(io::Error)`, malformed/other `ProbeFailure`, and `Cancelled`.
- The crate has only normal dependencies on `media-core`, `source-core`, `symphonia-demux`, and `thiserror`; guardrails reject player/app/UI/config/service dependencies.

## Symphonia boundary and invariants
- `symphonia-demux::probe_open_local_media_file` owns Symphonia 0.6 `Probe::probe`, default format/metadata options, initial metadata revisions, typed topology and duration extraction for an already opened `File`.
- Extension is only `Hint`, never an allowlist. Topology is counted from `CodecParameters::{Audio, Video}` and therefore null/unknown codec IDs remain admitted when their track type is known.
- The primitive never calls `FormatReader::next_packet`, creates no decoder, and skips playback-specific Matroska metadata/cue pre-scans. Existing `SymphoniaDemuxer` playback open/error semantics remain unchanged.
- Cancellation is checked before/after stages and before every actual read/seek through a cooperative file wrapper. `Interrupted` alone is I/O unless the shared token confirms cancellation. A single blocking regular-file syscall cannot be preempted mid-syscall; cancellation is observed at the next boundary.
- Symphonia 0.6 emits exact `core (probe): no suitable format reader found` only when no reader matches; only that sentinel maps to `UnsupportedContainer`. Reader-level `Unsupported` (for example malformed WAV missing a required chunk), decode/limit/reset/seek errors and `IoError(UnexpectedEof)` from a truncated header map to `ProbeFailure`; other unconfirmed I/O remains `IoFailure`.

## Scope boundary
- This primitive is for siblings, Manual Add, and demand metadata refresh. Explicit Play D64/D75 must use the Session 10C single prepared/open envelope and must not call discovery probe first.
- Directory enumeration, manifest limits/order, executor/jobs/progress, batching/admission, queue commit, app/player integration, and UI are not implemented in Session 08.

## Verification
- Hermetic coverage uses generated PCM WAV plus fake `FormatReader`: wrong/no extension, audio-only, video+audio null codec, metadata fields, duration/missing values, unsupported vs malformed vs I/O, missing/permission, cancellation, fingerprint/display filename, no A/V and `next_packet_calls == 0`.
- PASS: 8 playlist-discovery, 127 symphonia-demux, 34 media-core tests; strict focused Clippy; fmt; Rust 1.96 and MSRV 1.92 locked workspace checks; guardrails; Serena diagnostics/references; git diff check.
- Next allowed scope is Session 09 directory manifest foundation only.