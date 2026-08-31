# N05A provider-neutral web media catalog (2026-08-31)

## Архитектура

- `app-egui::web_media_catalog` больше не зависит от `service-ytdlp`. `WebMediaSelectionTarget` хранит только neutral `WebMediaSelection`: `Candidate`, `SeparateComponents`, либо честный inert `InstalledOnly` для direct/native HLS.
- Extractor adapter строит neutral catalog rows и отдельно закрытую reverse-route table. Provider-owned `YtDlpCandidateSelection` / `YtDlpComposedSelection` временно изолированы в `web_media_stream_model/catalog_routes.rs` как N05A→N05B compatibility bridge; catalog, coordinator и sidebar их не видят. N05B обязан удалить bridge вместе с legacy switch/reopen leakage.
- Для separate A/V одна video-строка получает максимум первую совместимую ranked audio-композицию. Декартово `video × audio` множество не публикуется.
- `WebMediaCatalogAttachment` и correlation допускают отсутствие exact parent/generation только для `InstalledOnly`. Extractor attachments сохраняют exact parent equality, semantic remembered preference и generation fences.
- Direct resource и native HLS после Installed публикуют один neutral `Automatic` row. Row видим, но `resolve_facet_action` fail-closed для catalog-а из одного choice и UI не создаёт action без parent generation.
- Read-only URL sidebar получает единый `WebMediaSourceReadProjection` с ingress kind, safe label, presentation и optional safe stream configuration. Catalog attachment передаётся coordinator-у через отдельный neutral boundary; read-only projection не несёт locator, request material или exact identity.
- `UrlSidebarModel::CatalogBacked` заменяет provider-named `YtDlp`; direct/native используют общий `DirectMedia { ingress, catalog, ... }`.
- Same-item transaction/reopen lifecycle не переписывался. Два необходимых compile-boundary изменения: optional catalog parent generation stale comparison и lookup legacy open intent через закрытую route table.

## Инварианты и тесты

- Single option остаётся видимым и inert.
- Stale catalog generation отсекает действие, которое на fresh generation действительно выбирает альтернативу.
- Direct/native coordinator rows не выдумывают parent generation.
- Target/attachment/correlation/read projection Debug не печатают raw exact/semantic/locator/request material.
- Focused composition helper прекращает audio scan после первой совместимой пары, закрепляя отсутствие Cartesian rows.
- `rg` audit: production `web_media_catalog` не содержит `service_ytdlp`/provider types; старые `UrlSidebarModel::YtDlp`, `UrlSidebarSourceProjection::YtDlp` и `native_hls_reopen` отсутствуют.

## Verification

- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.
- `cargo test -p app-egui --no-default-features --locked web_media_catalog::` — 12/12 PASS.
- `cargo test -p app-egui --no-default-features --locked web_media_open::catalog::` — 6/6 PASS.
- `cargo test -p app-egui --no-default-features --locked web_media_stream_model::tests::` — 15/15 PASS.
- Focused direct/native read-projection, inert sidebar and same-item lifecycle tests — PASS.
- `cargo clippy -p app-egui --all-targets --all-features --locked -- -D warnings` — PASS.
- `cargo check --workspace --all-targets --all-features --locked` — PASS.
- Serena final diagnostics для catalog, coordinator, stream model/routes, state, media-open projection и sidebar — clean.
- По §6.3 full workspace tests, public-media, GUI и hardware проверки в этой session не запускались.

Связанные memories: `mem:core`, `mem:app-egui/sidebar-controller`, `mem:app-egui/web-media-picker-slice-g-2026-07-26`, `mem:media-services/native-web-ingress-n01-2026-08-31`, `mem:media-services/native-web-ingress-n04-2026-08-31`.