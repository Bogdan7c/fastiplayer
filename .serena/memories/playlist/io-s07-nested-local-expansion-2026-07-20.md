# S07 bounded nested local expansion (2026-07-20)

## Ownership и public boundary
- `playlist-io` теперь является единственным neutral owner filesystem I/O и recursive local-only expansion для `.m3u`, `.m3u8`, `.xspf`; network fetch/service admission/queue mutation/Item ID allocation сюда не входят.
- Public entry point: `expand_local_playlist(LocalPlaylistExpansionRequest)`. Request явно несёт absolute reversible root path, `LocalPlaylistExpansionLimits`, per-document `M3uParserLimits`/`XspfParserLimits` и per-request `LocalPlaylistExpansionCancellation`.
- Реализация разделена на `src/local_expansion/engine.rs` (filesystem/DFS/accounting) и `model.rs` (public types), central `lib.rs` остаётся фасадом.

## Identity, tree и order
- `std::fs::canonicalize` используется только для transient active DFS ancestry `HashSet<PathBuf>`; canonical path никогда не записывается в preview/durable locator. `ExpandedLocalPlaylistDocument::source_path()` и parser drafts сохраняют original reversible native locator, включая non-UTF.
- Active-stack, а не global visited set, даёт self/A→B→A cycle rejection и разрешает repeated non-cycle include. Order deterministic depth-first; `depth_first_entries()` предоставляет borrowed iterator без второго cached flattened Vec.
- Result сохраняет document tree. `IncludedDocument` остаётся на исходной позиции entry/track; failure/budget/cycle/cancel даёт `UnexpandedInclude` с original M3U draft либо XSPF track. Поэтому XSPF source cardinality и original non-overlapping group ranges не теряются. При неполном source traversal `source_complete=false`, XSPF groups не публикуются как будто они валидны для усечённого списка.
- XSPF parser-side admission не добавлен: recurse разрешён только для одного unambiguous `file:` location с supported playlist extension. 0/N или multiple ordered candidates остаются `XspfTrack` для будущего S08 app locator-registry choice.

## Budgets, cancellation и safety
- Aggregate limits отдельно считают maximum depth (root=0), admitted documents, accepted total document bytes, retained leaf items и retained diagnostic details. `LocalPlaylistExpansionSummary` lossless хранит total/omitted diagnostics и typed depth/document/byte/item truncation counters независимо от detail cap.
- Filesystem read bounded до `min(remaining aggregate bytes, format-owned parser document cap)` плюс один sentinel byte; partial prefix никогда не передаётся parser-у. Это не позволяет caller-у с огромным aggregate limit обойти M3U/XSPF hard document cap.
- Cancellation проверяется только между documents; blocking read не заявлен interruptible. Каждый request создаёт отдельный Arc/AtomicBool token, поэтому stale token не отменяет новый request.
- Network URL, даже с playlist extension, остаётся leaf и никогда не fetch-ится. Local HLS возвращает typed `LocalHlsUnsupported`, root/include остаётся unexpanded, segment URI не становятся items.
- Errors/issues не содержат raw path/URL; raw reversible identity доступен только через explicit source/draft accessors.

## Focused coverage и verification
- `tests/nested_local_expansion.rs`: self-cycle, A→B→A, repeated non-cycle DFS, network leaf, XSPF local include/network leaf, symlink alias cycle, dangling canonicalization failure, non-UTF M3U/XSPF bases, depth/document/byte/item/diagnostic summaries, per-format read cap и local HLS.
- Internal reader seam tests cancellation exactly between root/child documents и stale-token isolation.
- PASS: 60 `playlist-io` tests total (2 unit + 58 integration), strict crate all-targets Clippy `-D warnings`, strict rustdoc, Rust 1.96 workspace locked check, Rust 1.92 focused all-targets locked check, rustfmt, refactor guardrails и diff check.
- `scripts/pre-pr-checks.sh` останавливается до Rust checks на прежнем coverage-policy inventory gap: `bounded-xml-reader`, `playlist-io`, `atomic-file-store`, `web-media-core` не классифицированы. S07 coverage policy не менял.

Related: `mem:playlist/core`, `mem:playlist/discovery`, `mem:playlist/io-s05-m3u-hls-2026-07-20`, `mem:playlist/io-s06-xspf-2026-07-20`.