# S17 topology-first Add URL collection import (2026-07-20)

## Ownership и flow
- `playlist_runtime::url_import` — новый process-lifetime owner toolbar Add URL для yt-dlp. Он хранит monotonic exact generation, один reusable worker, один заменяемый latest pending request и один generation-tagged terminal slot; renderer/UI и winit event payload не владеют raw locator/topology.
- `url_service_adapter::StartupUrlLocator::yt_dlp_topology_locator()` передаёт уже classified typed locator без второго parser-а. Direct-media-first routing не изменён: explicit direct media URL продолжает existing single append; только yt-dlp locator запускает topology job.
- Production worker вызывает существующий S15 `extract_yt_dlp_topology_with_config`, затем чистый S16 `map_yt_dlp_topology_to_playlist_drafts`, и передаёт ID-less `PlaylistImportDraft` в единственную S08 transaction с `PlaylistImportIntent::AppendToQueue`.
- Video topology становится Single preview; collection становится ordered Singles/Compounds preview; `multi_video` сохраняется first-class Compound. S08 остаётся единственным owner-ом capacity prefix, unavailable payload, structural-revision validation, Item/Group allocation и queue commit.

## Lifecycle, stale и privacy
- Rapid submit никогда не создаёт unbounded threads/processes: новый generation cooperative-cancel-ит running extraction и перезаписывает ещё не начатый latest request. Worker публикует completion только при exact current generation; submit/cancel/shutdown также очищают уже опубликованный stale slot.
- Общий `supersede_playlist_import_flow` теперь отменяет file-import job, active URL topology job и staged preview. New URL/main-open/row-play/structural replacement/shutdown поэтому не могут воскресить старый URL preview.
- `PlaylistShutdownReport` содержит отдельный `url_import` outcome. Shutdown закрывает generation, будит idle worker и join-ит его в общем `ShutdownDeadline`; timeout/panic остаются typed. Resolver panic изолируется, а poisoned worker mutex обрабатывается fail-closed без `PoisonError::into_inner`.
- Exact user locator остаётся только в typed service locator/durable payload. Query/userinfo-sensitive root передаёт count в прежний aggregated `SensitiveDurableLocatorPersistence` confirmation; Add URL никогда не добавляет `QueueReplacement` reason и никогда не Replace.
- App-visible errors generic/redacted; raw locator, service payload, stdout/stderr и config secrets не логируются.

## Queue/playback invariants
- Topology extraction/mapping и preview не меняют queue/current/player. Continue commit добавляет accepted whole-entry prefix и не выбирает current, не запускает media open/playback и не меняет active media.
- Metadata-only patch во время extraction не инвалидирует будущий append preview, потому что staging фиксирует актуальную structural revision после exact completion; существующий metadata cache сохраняется.
- Старый immediate yt-dlp append + post-commit metadata-enrichment continuation удалён. Visible-row manual yt-dlp metadata refresh остаётся отдельным discovery boundary; topology import использует metadata snapshot S16 и не запускает второй metadata process.

## Focused tests и verification
- Focused tests: `crates/app-egui/src/playlist_runtime/url_import/tests.rs`. Покрыты rapid submit/latest fencing, cancel, bounded shutdown, poison fail-closed, yt-dlp Video Single, Compound/group, unavailable part, exact sensitive acknowledgement, whole-group capacity, metadata patch и отсутствие current/playback mutation.
- PASS: `cargo +1.96.0 test -p app-egui --all-features` (783 tests), strict app all-target/all-feature Clippy, Rust 1.96 locked workspace all-features check, Rust 1.92 locked workspace check, rustfmt, refactor guardrails, `git diff --check`, `cargo deny check`.
- Serena diagnostics чисты для touched existing files и нового tests file; `url_import.rs` получил stale rust-analyzer E0583 на существующий `url_import/tests.rs`, опровергнутый focused/full Cargo test compilation.

Related: `mem:app-egui/playlist-import-s08-2026-07-20`, `mem:app-egui/ytdlp-topology-drafts-s16-2026-07-20`, `mem:app-egui/queue-replacement-confirmation-s14a`, `mem:app-egui/wake-runtime-s10a`, `mem:app-egui/ytdlp-playlist-metadata-2026-07-17`, `mem:media-services/ytdlp-topology-s15-2026-07-20`.