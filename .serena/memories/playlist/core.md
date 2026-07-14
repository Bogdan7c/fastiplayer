# Playlist core

Session 05 completed PASS on 2026-07-14. This memory complements `mem:core` and the handoff in `user/playlist_queue_implementation_plan.md`.

## Ownership and dependency boundary
- `playlist-core` is the neutral domain owner for stable playlist row identity, canonical order, the monotonic allocator, validated traversal current, and atomic queue mutations.
- It has exactly three normal dependencies: `media-core` for neutral metadata vocabulary, std-only `natural-sort-key` for the shared compact prepared filename comparator, and `rand` for production shuffle entropy plus injectable deterministic RNG boundaries.
- It does not depend on serde, UI/egui, player-core, filesystem I/O/discovery, service crates, config, async runtimes, demuxers, or concrete backends.
- `scripts/check-refactor-guardrails.py` treats it as a required contract crate and allows only `media-core`, `natural-sort-key`, and `rand`.

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

## Deterministic shuffle traversal (Session 04)
- `PlaylistQueue` keeps canonical order unchanged and owns optional enabled `ShuffleTraversal`; Off discards it, On preserves current and starts a new permutation of the other IDs. Persisted-idle enabled state (`current=None`) requires every canonical ID exactly once in upcoming, uses first upcoming for manual Next, and returns typed no-item for Previous.
- `ShuffleTraversal` owns Arc-backed ordered upcoming, factual repeated-visit history, back/forward cursor, and `MAX_SHUFFLE_HISTORY_ENTRIES = 1024`. Manual Play creates a factual visit and branches away from the forward tail; Previous/forward cursor moves do not create fake visits. RepeatQueue makes a new permutation and avoids last→same first when len>1.
- Production methods use automatically seeded `rand::rng()`; `*_with_rng` variants accept deterministic seeded sources. Batch add performs one O(N+K) random merge preserving old-upcoming relative order. `remove_batch`/`remove_others`, Clear, and current removal use bounded retain/rebuild paths and repair history/upcoming/cursor together; canonical reorder leaves traversal untouched.
- `ManualNavigationPreview` owns a shared/COW shuffle base plus bounded speculative path. Fast Next consumes candidates only inside preview; successful latest commit publishes intermediate consumption but only origin→latest factual history. Backtrack restores speculative candidates. D55 failure retains the same uncommitted preview/target; retry, Next, Previous, and discard preserve exact committed base until success.
- Serde/I/O-neutral `ShuffleTraversalSnapshot`, typed `ShuffleHistoryCursor`, and `PlaylistQueue::restore_with_shuffle` validate history cap, committed references, repeated factual history, cursor/current agreement, duplicate-free upcoming/current exclusion, and exact idle canonical coverage.

## Deterministic canonical sorting (Session 05)
- Public vocabulary is `PlaylistSortKey::{NaturalFilename, Title, Artist, Album, Duration, SmartSequence}`, `SortDirection`, intent `SortCanonicalQueue`, typed `SortCanonicalQueueOutcome`, and the sole mutation boundary `PlaylistQueue::sort_canonical`.
- Sorting is one-shot persistent canonical reorder only. It consumes cached metadata without probe/I/O/async/UI/config, prepares the selected normalized primary key plus natural fallback exactly once per item, and applies one in-place permutation only after the final order and next structural revision are known.
- A real reorder advances exactly one structural revision. Empty/single/already-sorted return `AlreadyInCanonicalOrder` without revision; D08 reservation returns `InstallCommitLinearizing`. Item IDs, allocator, optional traversal current, traversal/metadata revisions, and exact shuffle history/cursor/upcoming stay unchanged.
- Natural policy uses maximal ASCII digit runs without integer parsing (`2 < 10`; leading zeroes tie numerically), Unicode lowercase for valid UTF-8, and exact ASCII-folded native/foreign units for non-UTF values. Session 09 extracted this prepared comparison into the std-only `natural-sort-key` contract shared with discovery; exact filename units, exact locator identity, then Item ID remain `playlist-core`-owned tie-breakers.
- Title/artist/album use cached normalized strings with case-insensitive primary and exact fallback; artist is the ordered artists vector; duration is typed `MediaDuration`. Known values precede missing in both directions, and missing ties use ascending natural fallback.
- Smart Audio tuple is album/disc/track/title; Smart Video tuple is season/episode/title. Known tuple components precede missing components independently of direction. Ascending mixed smart order is Audio then Video, descending reverses those known groups; Unknown is always the missing group.
- Production ownership is split into `queue/sort.rs` (public boundary, metadata/smart comparator, permutation) and `queue/sort/natural.rs` (natural/non-UTF total key); both remain below the 700-800 line threshold.

## Verification and next scope
- 60 playlist-core tests, strict crate Clippy, fmt, Rust 1.96 and MSRV 1.92 locked workspace checks, and refactor guardrails passed for Session 05. Tests include every key/direction, non-UTF, missing/partial tuples, current/shuffle preservation, no-op/lock accounting, 10k deterministic characterization, one preparation per item, and comparator total-order laws.
- Session 06 persistence boundaries live in `mem:playlist/state`; Session 07 atomic save/durability, Session 08 single-file discovery probe, and Session 09 deterministic bounded directory manifest are complete. `playlist-core` itself remains serde/I/O-neutral. Next allowed work is Session 09A only: discovery executor/jobs/admission/readiness; UI/config and app-player integration remain outside the completed scope.