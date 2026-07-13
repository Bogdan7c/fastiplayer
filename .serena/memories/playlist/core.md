# Playlist core

Session 02 completed PASS on 2026-07-13. This memory complements `mem:core` and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and dependency boundary
- `playlist-core` is the neutral domain owner for stable playlist row identity, canonical order, the monotonic allocator, validated traversal current, and atomic queue mutations.
- It has exactly one normal dependency: `media-core`, solely to reuse `MediaDuration`, `DiscNumber`, `TrackNumber`, `TvSeasonNumber`, and `TvEpisodeNumber`.
- It does not depend on serde, UI/egui, player-core, filesystem I/O/discovery, service crates, config, async runtimes, demuxers, or concrete backends.
- `scripts/check-refactor-guardrails.py` treats it as a required contract crate and allows only `media-core`.

## Stable identity and allocator
- `PlaylistItemId` is an opaque `NonZeroU64`; zero is reserved. The first ID of a new lineage is 1.
- `PlaylistItemIdAllocator` owns `next_item_id`. App/discovery never reserve IDs separately and never compute max+1.
- Add/replace accepts `PlaylistItemDraft` without IDs, preflights the entire checked range, and returns `AllocatedPlaylistItemIds` only after successful commit.
- Remove, clear, and replacement never lower the watermark. Restore accepts a non-zero `NextPlaylistItemId` only when it is strictly greater than every unique restored ID. Capacity, arithmetic exhaustion, and collision are atomic failures.
- Hard queue capacity is `MAX_PLAYLIST_ITEMS = 50_000`.

## Canonical queue API
- `PlaylistQueue` owns `Vec<PlaylistItem>`, allocator, optional validated `TraversalCurrentItemId`, structural/traversal/metadata revisions, and at most one runtime reservation lock.
- Session 02 supports append one/batch, replace all, remove by ID, move by `ToFront`/`ToBack`/`Before(ID)`/`After(ID)`, clear, lookup, current validation/set/clear, and metadata patch batches.
- Manual duplicate locators are valid and receive independent IDs. Filesystem dedup/canonicalization is outside this crate.
- Current is only a persisted cursor referencing a committed row. It is not active media, pending target, `MediaInstanceId`, or player state. Removing current clears it and does not pick a successor.
- Navigation, repeat, shuffle, sorting, tombstones, Undo, I/O, JSON, config, async, and app controller policy are not present after Session 02.

## D08 reserved mutation boundary
- `ReservedQueueMutation` is an opaque intent built via named constructors for selecting a committed target or replacing with `before/current/after` drafts.
- `prepare_reserved_mutation` checks structural/traversal revisions, committed references, capacity, allocator range/collision, and future revisions before installing one lock.
- `PreparedQueueMutationToken` is opaque, non-Clone, non-serde, and does not expose proposed IDs. Prepare does not change watermark/current/revisions.
- While active, allocator/structural/traversal mutators return typed `InstallCommitLinearizing`; read-only queries and metadata-only patches remain allowed.
- Exact abort burns nothing. Exact `commit_reserved` has no business-error result and publishes IDs/queue/current/watermark in one step. A foreign/mismatched token is a fatal invariant diagnostic.

## Locator/privacy boundary
- `PlaylistLocator` is URL or local. Local is `LocalLocator::Native(PathBuf)` or reversible `ForeignPlatformPath` with platform plus UTF-8/bytes/wide/opaque units.
- Native/open identity is never derived through lossy UTF-8. Foreign raw bytes/wide units survive domain roundtrip. Discovery canonical keys are not stored.
- `SecretUrlLocator` has explicit redacted `Debug`/`Display`; userinfo, query, fragment, and path payload are hidden. Raw identity is available only through `expose_secret_for_open` and `expose_secret_for_persistence`.
- Public outcomes/errors use explicit safe formatting and do not embed raw locators.

## Cached metadata and patches
- `CachedPlaylistMetadata` owns fallback display name, media kind (Unknown/Audio/Video), optional duration/title/artists/album/disc/track/season/episode. Artists are bounded by `MAX_CACHED_ARTISTS = 32`; no comparator keys are stored.
- `LocalSourceFingerprint` is file size plus exact `SystemTime` mtime; it is best-effort cache invalidation, not a content hash.
- Each `PlaylistMetadataPatch` carries Item ID, expected locator, expected fingerprint, and replacement cache.
- Batch outcomes distinguish Applied/NoChange/NotFound/SourceMismatch. Matching updates publish atomically with one metadata revision and do not alter order, current, or structural/traversal revisions.

## Canonical navigation and repeat (Session 03)
- `RepeatMode` is `StopAtEnd`, `RepeatQueue`, or `RepeatOne`; it remains domain state, while stop-after-current, Play/Pause intent, D17 restart threshold, active identity, and player snapshots stay outside `playlist-core`.
- Automatic clean `Ended` uses `AutomaticEndedIntent` and returns typed `OpenItem`, `ReplayCurrent`, or `Stop`. `RepeatOne` always produces `ReplayCurrent` for the validated current ID without locator reopen; `StopAtEnd` stops at last, `RepeatQueue` wraps last to first, and persisted `current=None` produces a typed stop rather than inventing an active item.
- Manual `Next`/`Previous` use `ManualNavigationIntent` and return `OpenItem` with an opaque `ManualNavigationPreview` or typed `NoItem`. Manual traversal ignores `RepeatOne` inside canonical order, wraps only for `RepeatQueue`, maps idle `Next` to first and idle `Previous` to no-item, and reports speculative return to committed origin separately so the controller cancels pending install.
- `ManualNavigationPreview` is runtime-only and non-Clone. It stores committed origin, latest desired target, structural/traversal revision base, and typed D55 failed-target state. A→B→C/backtrack queries, failure marking, and discard do not mutate committed traversal.
- Manual install reuses the existing D08 reservation owner. `prepare_manual_navigation` wraps `PreparedQueueMutationToken`; prepare failure returns both the exact typed D08 reason and the preview, abort returns the preview, failure returns it with an awaiting-user marker, and only `commit_manual_navigation` after correlated external success publishes the latest target current.
- Structural/traversal changes invalidate a preview; metadata-only revision changes do not. No shuffle/RNG/history/upcoming behavior is present yet.

## Verification and next scope
- 33 playlist-core tests, strict crate Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails, and git diff check passed for Session 03.
- All production modules remain below 800 lines: navigation is 586 lines, central queue/mod.rs is 682, and the pre-existing typed outcomes module remains 779.
- Next allowed work is Session 04 deterministic shuffle traversal on top of the opaque preview boundary. Do not begin sorting or app/player integration.