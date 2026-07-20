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
- Session 06 persistence boundaries live in `mem:playlist/state`; Session 07 atomic save/durability, Sessions 08–09A discovery, Sessions 10A–10D runtime/media-open foundations, Sessions 11A–11C manual controller foundations, and Session 12 automatic lifecycle are complete. `playlist-core` remains serde/I/O-neutral; `queue::automatic` owns opaque fixed-snapshot traversal tokens, Session 15 added exact-revision stable-anchor discovery insertion, and Session 16 added `append_capped_tail`: one O(N+K) responsive-shuffle merge accepts only the caller-ordered D67 capacity prefix, returns rejected count, and allocates no IDs/revision at zero capacity. `PlaylistMetadataPatch::refreshed_local` atomically replaces matching fingerprint+metadata; ID/locator/old-fingerprint stale guards and metadata-only revision semantics remain unchanged. App orchestration is documented in `mem:app-egui/playlist-discovery-s15`. Sessions 12A–18 are complete, including transactional metadata Sort, startup/restore orchestration and read-only virtualized Playlist UI. `playlist-core` boundaries remain unchanged; Session 18A in `app-egui` wired main transport controls/hotkeys/global wait and Undo without moving traversal or UI ownership into this crate. Sessions 18B–20 are complete without changing `playlist-core`: desktop/MPRIS remains process-owned, toolbar/forms/progress actions are app-egui adapters, and Session 20 row interactions reuse the existing stable-ID move/bulk removal boundaries. The next allowed playlist work is Session 21 hardening.

## Session 11B controller integration note (2026-07-14)
- Manual one-step app navigation preserves the opaque `ManualNavigationPreview`/`PreparedManualNavigationToken` through the D08 guard until exact Installed. Abort returns/discards it without committed traversal mutation; commit alone publishes target and exact shuffle history/upcoming state.
- A reserved select of the already-current Item ID while shuffle is enabled represents a new factual reinstall visit: it advances traversal revision and appends the factual history entry even though Item ID is unchanged. Structural revision, allocator, and canonical order do not change.
- Manual transport intent, D17 threshold, D50 wait, D52 correlation, stop-after-current, and app Stopped disposition remain app/player responsibilities documented in `mem:app-egui/playlist-controller-s11b`. Fast repeated target preview stays Session 11C.


## Session 12A removal snapshot and current semantics (2026-07-15)
- `queue::removal` owns opaque `PlaylistRemovalSnapshot`, typed `RemovalCurrentOutcome::{Preserved,Detached}`, and restore-as-new-mutation. Removing the committed current persists `None` without choosing a successor.
- Snapshot restore requires the immediately post-removal structural revision, unchanged metadata/allocator state and no active D08 reservation; it advances structural/traversal revisions instead of rolling them back.
- `PlaylistItem` stores immutable locator + cached metadata in a private Arc payload. Removal snapshots share every heavy payload; metadata mutation uses `Arc::make_mut`, so later metadata cannot mutate the pre-removal snapshot.
- Shuffle history/upcoming remain domain-private and Arc-backed. Tombstone continuation uses the opaque Session 12 automatic plan plus `revalidate_automatic_traversal`; current is still committed only by exact Installed token.
- App/runtime tombstone and Undo lifecycle details are in `mem:app-egui/playlist-controller-s12a`.
## Session 16A transactional prepared Sort
- `PlaylistQueue::canonical_sort_snapshot` returns an immutable Arc-sharing snapshot with structural/traversal/metadata revision capture. `CanonicalSortSnapshot::prepare` is pure/cancellable background work: it applies candidate metadata patches only to cloned Arc-backed items, builds each normalized/natural key once, and produces a stable prepared permutation with O(N log N) comparisons and O(N) memory.
- `preflight_prepared_canonical_sort` revalidates structural+metadata revisions, exact membership/order, permutation integrity, D08 reservation, and revision counters before producing `PreparedCanonicalSortCommit`. The following commit is infallible and publishes matching metadata plus canonical order without an intermediate state. Reorder advances structural revision once; metadata advances metadata revision once; traversal/current/shuffle remain Item-ID based and unchanged.
- Metadata patch preparation now creates one Item ID→index map and stages the full batch before mutation, making combined Sort and D44 salvage O(N+patches) instead of repeated linear item lookup. `preflight_metadata_patch_batch` returns opaque `PreparedMetadataPatchBatchCommit` only after revalidation and metadata revision preflight; `commit_metadata_patch_batch` is then infallible in the same serialized owner turn, and the compatibility apply method delegates to these phases. Missing/locator mismatch/fingerprint mismatch/no-change remain typed outcomes and cannot resurrect removed/replaced rows; revision exhaustion preserves cache and all revisions.
- Session 16A verification: 74 core tests, including cancel/stale atomicity, combined metadata+order, unchanged traversal and 50k once-per-item key/O(N log N)/Arc-sharing characterization; strict Clippy, fmt, locked workspace check and guardrails PASS.


## Atomic group reorder API (2026-07-18)
- `PlaylistQueue::move_items(&[PlaylistItemId], MoveItemIntent) -> MoveItemsOutcome` is the public group-reorder boundary. It validates empty input, duplicate/missing IDs, missing/selected anchors, D08 linearization and revision exhaustion without partial mutation.
- Caller order never defines the moved block: the queue performs one canonical scan and preserves the selected rows’ existing relative order. It builds the final order first, returns `AlreadyInPlace` for an identical permutation, and publishes at most one structural revision on `Moved`.
- Group reorder leaves traversal current/revision, allocator, metadata revision, item payloads and exact shuffle history/cursor/upcoming unchanged. The implementation is in `queue/reordering.rs`; focused tests include invalid/no-op atomicity, D08, revision exhaustion, discontiguous groups and the 50k capacity path.
- Verification after this change: 79 `playlist-core` tests PASS; strict workspace/all-targets/all-features Clippy, Rust 1.96 locked workspace check, rustfmt, refactor guardrails and diff check PASS.


## S01P/S01Q intent-based read boundary (2026-07-20)
- `queue/read.rs` владеет новым public read surface: borrowed opaque `iter_playable_items()`/`iter_playable_ids()` возвращают exact-size + double-ended iterators без раскрытия `std::slice::Iter`; stable lookup остаётся `PlaylistQueue::item(PlaylistItemId)`.
- `top_level_entry_count()` и `retained_item_count()` сейчас оба O(1) и равны flat `items.len()`, но закрепляют разные caller intents перед compound storage. Capacity/persistence accounting выбирает retained semantics; structural entry UI в будущей migration должен выбирать top-level semantics.
- `OwnedPlayableItemsSnapshot` — immutable Arc-backed flat playable projection только для ownership handoff. Он открывает iteration/count/stable lookup, но не slice, indexing или mutation API; queue reorder/metadata COW после capture не меняют snapshot. Background `CanonicalSortSnapshot` переиспользует этот boundary.
- Все `playlist-core` production algorithms/tests мигрированы с public `items()`/`len()` на intents. Observable canonical order, duplicates, stable IDs, allocator/revisions, shuffle/navigation semantics и reversible non-UTF locator identity не изменились; cached/parallel queue Vec, interior mutability и compound types не добавлялись.
- S01Q мигрировал все workspace callers в `app-egui`: view/UI count выбирает top-level intent, capacity/persistence/playable lifecycle — retained intent, traversal/lookup — borrowed playable iterator или stable-ID `item()`. Selection range validation использует revision guard + stable IDs + bounded iterator `skip/take`; removal fallback применяет локальную позицию только после owner-turn commit и не получает queue mutation authority.
- `PlaylistQueue::items()` и ambiguous production `len()` удалены. Нулевой workspace source audit охватывает все три crates, зависящие от playlist-core (`playlist-core`, `playlist-state`, `app-egui`); компиляция дополнительно доказывает отсутствие typed callers. Cached/parallel queue Vec, interior mutability, compound storage и новые app-owned snapshots не добавлялись.
- Observable canonical order, duplicate handling, Item IDs, allocator, structural/traversal/metadata revisions, shuffle/navigation/removal semantics и playlist-state schema v1 не изменились.
- Verification: 82 playlist-core, 40 playlist-state и 719 app-egui tests PASS on Rust 1.96; strict touched-crate all-targets/all-features Clippy, workspace Rust 1.96 check, focused MSRV 1.92 check, rustfmt, Serena diagnostics and refactor guardrails PASS. Dependency audit unchanged: only known quick-xml RUSTSEC-2026-0194/0195 blockers fail.

## S01A first-class compound identity/storage/capacity (2026-07-20)
- Canonical owner `PlaylistQueue` теперь хранит `Vec<PlaylistEntry>`, где `PlaylistEntry::{Single, Compound}` является единственным top-level order. `PlaylistEntryId::{Single(PlaylistItemId), Compound(PlaylistCompoundGroupId)}` разделяет playable и structural identity; compound variant boxed только для компактного enum layout.
- Независимый `PlaylistCompoundGroupIdAllocator` повторяет monotonic/no-burn семантику Item allocator и имеет собственный `NextPlaylistCompoundGroupId`. Общий `append_entries`/`replace_entries` preflight проверяет retained capacity, structural/traversal revision, Item IDs и Group IDs до публикации обоих watermarks/storage. Legacy `append_batch`/`replace_all` остаются single-only фасадами того же commit boundary.
- `PlaylistCompoundGroupDraft::new` возвращает typed `EmptyPlaylistCompoundDraft` до allocation; one-part group сохраняется compound. Committed group хранит safe `PlaylistLocator` root provenance, `CachedPlaylistMetadata` summary и ordered boxed parts. Каждая part имеет stable Item ID и immutable one-based `PlaylistCompoundMembership`/`PlaylistCompoundPartOrdinal`.
- `append_capped_entries` выбирает только caller-ordered top-level prefix: retained capacity считает parts, group никогда не режется, после первого не помещающегося entry отклоняется весь tail с exact entry/item counts. Exact-capacity group разрешена.
- S01P read surface теперь реально работает поверх nested storage: `iter_top_level_entries`/`iter_top_level_entry_ids` отделены от custom borrowed exact-size+double-ended `iter_playable_items`/IDs. Flat projection не хранится в queue; `OwnedPlayableItemsSnapshot` materializes только explicit ownership handoff и продолжает Arc-share payload-ы.
- Metadata patch plan теперь keyed by stable Item ID и применяет staged cache напрямую к nested entries за O(N+patches), сохраняя metadata-only revision semantics. Runtime removal snapshot захватывает entries и оба allocator-а; v1 `PlaylistQueueRestore` по-прежнему создаёт только Singles и initial Group allocator до persistence v2 session.
- Scope честно остановлен на S01A: complete group-safe structural intents/sort/discovery/Undo принадлежат S01B, а group-block shuffle/reservation/navigation — S01C; player identity и UI collapse state в `playlist-core` не добавлялись.
- Verification: 94 playlist-core, 40 playlist-state и 719 app-egui tests PASS; workspace all-features check, strict workspace all-targets/all-features Clippy, rustfmt, focused MSRV 1.92 check, diff check и refactor guardrails PASS.


## S01B group-safe structural mutations (2026-07-20)
- `remove`, `remove_batch`, `remove_others`, `move_item`, `move_items` и `MoveItemIntent::{Before,After}` принимают только `PlaylistEntryId`. Missing top-level identity и forbidden subordinate `Single(part_id)` различаются typed outcomes/errors; individual part remove/reorder остаётся вне v1.
- Shared internal `queue/structural.rs` владеет top-level lookup distinction. Single remove живёт в `queue/removal.rs`, multi/single reorder — в `queue/reordering.rs`, bulk remove + shuffle cleanup — в `queue/shuffle/removal.rs`; central `queue/mod.rs` снова ниже 700 строк.
- Whole-group removal derives every subordinate Item ID for current detachment and exact shuffle reference cleanup. Multi-move preserves canonical entry order and uses one `O(N+K)` map/scan; group parts never become independent selected entries.
- `StableInsertionAnchor` carries `Option<PlaylistEntryId>` and inserts only at top-level boundaries. Stale group anchors and part anchors are typed and allocator-atomic.
- Direct and prepared Sort operate on top-level entries. Compound keys come exclusively from `cached_summary` + root provenance, equal-key sort stays stable, part order/current/traversal/shuffle identity stays unchanged. Prepared plans validate Entry-ID membership/permutation while Item-ID metadata patches still apply to nested parts before commit.
- `restore_removal_snapshot` additionally proves current entries are an exact order-preserving deletion subset of the snapshot; this blocks unrelated one-revision reorder overwrite and restores exact Item/Group IDs, allocators, current and shuffle state.
- App controller selection preflight rejects partial compound coverage before snapshot/dirty/domain mutation and translates full playable selections to explicit Entry IDs in `O(N+K)`. Focused tests cover every boundary, stale/part anchors, current part, partial selection, full-group move/remove, part order and Undo IDs.
- Verification: 100 playlist-core tests and 721 app-egui tests PASS; focused compound suites PASS; strict Clippy, Rust 1.96 workspace check, focused MSRV 1.92 check, rustfmt, guardrails, diff check and Serena diagnostics PASS.


## S01C part traversal, reservation и group-block shuffle (2026-07-20)
- Canonical derived traversal сохраняет structural `Vec<PlaylistEntry>` S01A/B неизменным: current — exact part Item ID; manual/automatic Next/Previous используют flat source-order parts только как projection. Исправлен старый single-only internal automatic index lookup.
- Новый маленький `queue/traversal.rs` владеет intent helpers: first playable Item ID entry, next part внутри entry и fixed-chain Item→Entry membership. `navigation.rs` не получил storage knowledge и остаётся ниже owner boundaries.
- `ShuffleTraversal`/`ShuffleManualPreview` хранят `history: ItemId` и `upcoming: EntryId`. Speculative step помечает, consumed ли top-level entry: backtrack возвращает только реально consumed block, internal part step не меняет upcoming. Enable на middle part исключает текущий group block, но Next проходит suffix; Previous не выдумывает earlier parts. RepeatQueue shuffle генерирует permutation top-level entries и избегает last-entry→same-first при числе entries > 1.
- Direct Play/reserved select связывают target Item ID с owning Entry ID: whole block удаляется из upcoming, history получает ровно factual Installed part. Same-part reserved reinstall в compound создаёт ровно один дополнительный factual visit. Abort/failure/stale preview и D08 lock не публикуют traversal delta; structural remove во время Ready остаётся `InstallCommitLinearizing`.
- Automatic fixed snapshot сохраняет eligible Item IDs и derives surviving eligible Entry IDs. Failed parts не входят в history; chain идёт по remaining parts, затем следующему shuffled block, исключает late additions и целиком отфильтровывает removed group.
- Append/discovery random merge принимает allocated Entry IDs; remove filters upcoming Entry IDs и history Item IDs отдельно; sort/move сохраняют snapshot identities. Focused regression доказывает uniqueness/committed membership upcoming, отсутствие subordinate `Single(part_id)` и неизменный internal part order после sort/discovery/remove/move.
- Public `ShuffleTraversalSnapshot`/restore error vocabulary теперь group-aware: factual history Item IDs, upcoming Entry IDs. Idle restore требует exact canonical Entry-ID coverage; active current запрещает presence owning Entry ID в upcoming.
- Focused coverage находится в `queue/group_traversal_tests.rs`; app exact Ready→Installed part coverage — `app-egui/.../controller/manual_navigation/tests.rs`. Verification: 106 core tests + соседние 41 state/722 app tests, strict Clippy/check/MSRV/fmt/guardrails/diagnostics PASS.


## S01D neutral payload и ID-less import drafts (2026-07-20)
- `playlist-core` теперь публично владеет нейтральными `PlaylistPlaybackSpan`, `PlaylistAncillaryTrackHint`, `PlaylistImportProvenance` и `DurableReopenLocator`; реализация разделена на `playback_span.rs`, `payload.rs` и `import.rs`, без новых dependencies и без serde/service/app/player/I/O boundary.
- `PlaylistPlaybackSpan` хранит absolute start + optional exclusive end, отвергает zero/reversed span и строит end/duration только checked arithmetic.
- `DurableReopenLocator` различает exact local `LocalLocator`, exact redacted `SecretUrlLocator` и bounded opaque service payload v1. Service owner grammar/bytes, payload bytes и version explicit; unknown version typed rejected. Stable webpage/original/extractor identities разрешены, а `formats[].url`, manifest/fragment/key URL, signed endpoint, headers, cookies и authorization/session categories typed rejected до сохранения bytes. Debug/Display/errors не раскрывают paths, URLs, stable identities или service payload.
- `PlaylistAncillaryTrackHint` хранит bounded semantic identity/language/display/manual-or-automatic/embedded-or-external durable origin/service format identity; item-level hint count bounded.
- `PlaylistImportProvenance` хранит только durable root, source kind и optional non-zero source ordinal. Свободной child identity строки намеренно нет: stable child identity имеет единственный owner в `DurableReopenLocator`, что не оставляет обходного канала для ephemeral endpoint.
- `PlaylistSingleImportDraft` и `PlaylistCompoundImportDraft` — отдельные ID-less parser/service-to-future-transaction contracts. Они несут metadata/span/ancillary/provenance/availability, не выделяют Item/Group IDs и не меняют S01A-C queue storage/traversal. One-part compound сохраняется compound; zero/oversized group rejected; unavailable child обязан всё равно иметь durable locator.
- S01D не добавляет mapping import drafts в canonical queue и не начинает playlist-state v2/S08 transaction. Verification: 119 playlist-core tests, strict Clippy, Rust 1.96 workspace all-features check, MSRV 1.92 all-targets check, fmt, guardrails, diff check и Serena diagnostics PASS.


## S01G compound-core hardening gate (2026-07-20)
- Полный symbol/reference audit всех public/internal `PlaylistQueue` mutation families не обнаружил escaped owner mutator или необходимость новой feature logic/API. Structural identity остаётся `PlaylistEntryId`, playable current/history — exact `PlaylistItemId`, shuffle upcoming — top-level Entry ID; discovery остаётся neutral.
- Новый allocator characterization доказывает независимые monotonic/no-burn Item и Group high-watermarks через Clear, compound `replace_entries`, legacy single-only reserved strong-install replacement и следующий compound append.
- Новый read characterization чередует `next`/`next_back` по Single + one-part + many-part storage и доказывает точный remaining `len()`. Source guardrail фиксирует единственный direct `PlaylistQueue` Vec как `Vec<PlaylistEntry>`, отсутствие direct Arc/owned playable cache и автоматически ограничивает размеры всех production queue-модулей.
- 122 core + 41 state + 722 app tests, strict Clippy, Rust 1.96 workspace check, MSRV 1.92, fmt, guardrails, diff check и diagnostics PASS. Cargo-deny unchanged FAIL только на quick-xml advisories. Detailed evidence: `mem:playlist/compound-hardening-s01g-2026-07-20`.

## S02 persistence restore/durable payload boundary (2026-07-20)
- `playlist-core` остаётся serde/I/O-neutral, но получил exact persistence restore vocabulary: `RestoredPlaylistEntry::{Single,Compound}`, `RestoredPlaylistCompoundGroup` и `PlaylistQueueRestore::from_entries`. Restore валидирует retained capacity, unique Item/Group IDs, exact current part и оба allocator watermark атомарно; legacy `PlaylistQueueRestore::new` по-прежнему создаёт только Singles с initial Group watermark.
- Restore owner вынесен в `queue/restore.rs`; persistence entry records — в `entry_restore.rs`. Это сохраняет central module-size guardrail; `queue/mod.rs`, `entry.rs` и `outcomes.rs` снова ниже лимитов.
- Canonical `PlaylistItem` и `PlaylistCompoundGroup` теперь могут хранить optional validated durable import payload, не меняя legacy locator/open behavior. Existing local/url/group constructors создают `None`; builders `with_durable_payload` используются persistence restore и будущей import transaction.
- `PlaylistSingleDurablePayload`/`PlaylistCompoundDurablePayload` отделяют reopen/span/ancillary/provenance/availability от cached metadata и ID allocation. S01D import drafts переиспользуют эти payload types, поэтому persistence и будущий transaction не дублируют semantic fields.
- Metadata patches/Undo Arc sharing, traversal, structural identity и app open boundaries не изменены. S02 verification: 122 core tests, strict Clippy, Rust 1.96 workspace check, MSRV 1.92, fmt и guardrails PASS.


## S05 playlist-io consumer note (2026-07-20)
- Новый neutral `playlist-io` переиспользует S01D `PlaylistSingleImportDraft`/`PlaylistImportProvenance` как ID-less generic M3U preview payload. Это dependency `playlist-io -> playlist-core`; reverse dependency, queue handle, allocation или mutation authority не добавлены.
- M3U/M3U8 provenance различается через существующий `PlaylistImportSourceKind`; positive EXTINF может заполнить cached `MediaDuration`/title, negative duration остаётся parser-owned unknown hint и не превращается в span/end. Полный parser contract: `mem:playlist/io-s05-m3u-hls-2026-07-20`.

## S06 playlist-io XSPF consumer note (2026-07-20)
- XSPF parser возвращает отдельную ID-less `XspfPlaylist` preview model с flattened tracks, ordered location candidates и optional group ranges; он намеренно не строит `PlaylistSingleImportDraft`/`PlaylistCompoundImportDraft`, потому что source-neutral admission/transaction принадлежит S08.
- XSPF duration хранится только как `MediaDuration` hint и не создаёт `PlaylistPlaybackSpan`. Parser не получает allocator, queue handle, stable Item/Group IDs или mutation authority.
- `XspfExportLocation` читает только explicit durable locator exposure для URI eligibility; queue snapshot/compound export transaction остаются S10. Полный contract: `mem:playlist/io-s06-xspf-2026-07-20`.

## S07 nested local expansion consumer note (2026-07-20)
- `playlist-core` API/queue/storage/allocation authority не изменились. `playlist-io` рекурсивно строит ID-less document tree из существующих `PlaylistSingleImportDraft` и XSPF track/group models; Item/Group IDs и canonical queue commit остаются будущей S08 transaction.
- Canonical filesystem path не становится `DurableReopenLocator`: он transient только в active DFS cycle stack. Original native/non-UTF locators и failed-include payload сохраняются reversible. Полный S07 contract: `mem:playlist/io-s07-nested-local-expansion-2026-07-20`.

## S08 import materialization и replacement-detached navigation (2026-07-20)
- `PlaylistImportEntryDraft::into_queue_draft` — public ID-less materialization boundary перед app transaction commit. Local/URL durable locator становится legacy operational locator; opaque service child использует только local/URL provenance root, иначе typed `PlaylistImportMaterializationError`. Durable item/group payload сохраняется; IDs не выделяются до `PlaylistQueue` commit.
- `PlaylistQueue::begin_replacement_detached_navigation` — intent-named manual preview только для app replacement disposition: Next выбирает первый, Previous последний source-order Item ID. Shuffle preview удаляет owning top-level Entry ID из upcoming, сохраняет compound block semantics и добавляет factual visit только после exact commit.
- Full app transaction/lifecycle и verification: `mem:app-egui/playlist-import-s08-2026-07-20`.


## S10 playlist-io export consumer note (2026-07-20)
- `PlaylistExportSnapshot::capture(&PlaylistQueue, Full|Selected)` находится в downstream `playlist-io` и использует только public immutable top-level read boundary; `playlist-core` не получил serializer/I/O/service dependencies или новый mutator.
- Snapshot клонирует selected `PlaylistEntry` payload-ы для ownership handoff, сохраняет canonical order/duplicates/whole compound parts и после capture не держит queue handle. Capture/preflight/serialize не меняют structural/traversal/metadata revisions, current, allocators или shuffle state.
- Полный locator/format/service contract: `mem:playlist/io-s10-export-2026-07-20`.
