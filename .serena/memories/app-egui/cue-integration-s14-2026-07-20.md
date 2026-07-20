# S14: CUE app integration и export (2026-07-20)

## Архитектура
- `playlist-core::cue_semantics` владеет durable exact CUE identity: `PlaylistCueFrameIndex`, `PlaylistCueTrackExportSemantics`, FILE type и fail-closed document eligibility. Exact 75-fps frames не восстанавливаются из nanosecond `PlaylistPlaybackSpan`.
- `PlaylistSingleDurablePayload` хранит optional CUE semantics. `with_cue_export_semantics` разрешает attachment только при CUE provenance, наличии playback span и exact совпадении INDEX 01 с началом span.
- `playlist-state` schema v2 получила additive optional `cue_export_semantics`; старые v2 документы читаются как `None`, новые round-trip сохраняют exact frames и fail-closed eligibility.
- `playlist-io` CUE parser одновременно строит playback span и exact export semantics. Unknown command/sub-index не ломает import, но маркирует весь source document export-ineligible.
- `playlist-io::export::cue` владеет pure scope eligibility/preflight/serializer. Export допустим только для top-level Singles из одного CUE root, с последовательными track numbers, exact boundaries, local UTF-8 paths и представимыми metadata. Последний выбранный track обязан иметь EOF end; поэтому exact EOF suffix допустим, а обрезанный prefix — нет.
- `app-egui` import dialog маршрутизирует `.cue` в bounded S12 parser; extension лишь выбирает parser, content validation остаётся authoritative.
- `PlaylistRuntime::media_open_intent_for_planned_install` возвращает locator и neutral `MediaPlaybackWindow` под одним exact revision/item guard. App затем строит физический source request и оборачивает его в `MediaOpenSourceRequest::PlaybackWindow`; `player-core` не знает CUE типов.
- CUE availability для full/selected scope кешируется в `playlist_runtime/export_io/cue_availability.rs` по `PlaylistViewRevision`, содержащей и queue, и selection presentation changes. Renderer получает typed availability и safe reason; toolbar slots/geometry не изменялись.

## Инварианты
- Current остаётся exact CUE track `PlaylistItemId`; clean Ended и RepeatOne используют существующую item lifecycle policy.
- CUE semantics кладутся одновременно в durable queue payload и через open intent проецируются в reconstructible active-open playback window.
- Resume/reopen/settings/detached suspend сохраняют window через существующий S13 `PreparedMediaOpen`/`ActiveMediaSource` contract.
- CUE serializer никогда не делает lossy path conversion; кавычки/CR/LF в FILE или metadata fail-closed.

## Ключевые тесты
- `crates/playlist-io/tests/cue_export.rs`: full/selected eligibility, exact round-trip, unknown command rejection.
- `crates/app-egui/src/playlist_runtime/controller/automatic_lifecycle/tests.rs`: CUE Ended→next и RepeatOne exact current item.
- `crates/app-egui/src/playlist_runtime/import_io.rs`: CUE route/content mismatch.
- `crates/app-egui/src/playlist_runtime/transport_execution.rs`: durable span→neutral open intent.
- `crates/playlist-state/src/v2_tests.rs`: exact CUE schema-v2 round-trip.

Связанные memories: `mem:app-egui/media-open-coordinator-s10c`, `mem:playlist/resume-position-sidecar-2026-07-19`, `mem:app-egui/playlist-ui-s20`, `mem:playlist/io-s12-cue-2026-07-20`.