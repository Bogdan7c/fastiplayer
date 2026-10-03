# media-source-open (создан 2026-10-03, session-01 выноса web-media)

Library crate `crates/media-source-open` (lib `media_source_open`): открытие источников без UI. Вынесен из `app-egui` как рефакторинг границы — поведение, тексты ошибок, redaction, generation/identity fences не менялись. План серии: `user/web-media-extraction/` (приватно).

## Модули (объявлены в корне `pub mod` под прежними именами → пути тестов `web_media_quality::tests::...` сохранены)
- `local_media` — открытие локальных файлов (`prepare_local_file`, `open_local_demuxer_from_source`, `LocalDemuxOpenError`, `SUPPORTED_LOCAL_MEDIA_EXTENSIONS`); тестовая TS-фикстура — копия в `local_media/ts_fixture.rs`.
- `direct_progressive_open` — прямые HTTP/FTP (`classify_direct_media_url`, `open_direct_media`, `DirectProgressiveOpenResult`); зависит от `web_media_vod_recovery` и `web_media_demux_registry`.
- `web_media_vod_recovery` (`VodEndpointRecoveryAttachment`), `web_media_demux_registry` (`WebDemuxComposition` — поля `registry`/`capabilities` публичные, как были pub(crate); кандидат на intent-методы отдельной задачей), `web_media_extractor_adapter` (`ExtractorCatalogProjection`/`ExtractorAdapterProjection`), `web_media_hls_subtitles`, `web_media_quality`, `web_media_adaptive_config`.
- Видимость: `pub` только у того, что зовёт app-egui; остальное `pub(crate)`.

## Границы и проверки
- Запрещённые зависимости (`MEDIA_SOURCE_OPEN_FORBIDDEN_DEPENDENCIES` в `scripts/check-refactor-guardrails.py`, тест `test_media_source_open_stays_below_ui_and_render`): app-egui, egui*, winit, wgpu, render-*, ui-artwork-egui.
- Feature `test-fixtures`: открывает `InstalledHlsSubtitleRendition::fixture` для тестов app-egui; включается только из `[dev-dependencies]` app-egui. Рабочую сборку проверяет `cargo check -p app-egui --no-default-features`.
- Сторож provider DTO yt-dlp разделён: в crate — `web_media_extractor_adapter::tests::provider_dtos_stay_inside_exact_extractor_adapter_allowlist` (allowlist = только адаптер); в app-egui — `crates/app-egui/src/extractor_provider_dto_guard_tests.rs` (исходный allowlist без адаптера + `active_source_shape_excludes_ephemeral_endpoint_header_and_cookie_types`).
- Зарегистрирован в: workspace members/deps, `scripts/ci-checks.sh` (cargo-machete inventory), `coverage/policy.json` → `informational_crates`, ARCHITECTURE.md (таблица владельцев).
- `flv-demux` больше не зависимость app-egui (был нужен только demux_registry).

## Осталось в app-egui (переедут позже)
- `web_media_hls_refresh`/`web_media_dash_refresh` — зависят от `web_media_hls_open`/`web_media_dash_open` (сессия 3).

## Coverage baseline — известное красное состояние
- Решение владельца 2026-10-03: пересъёмка `coverage/baseline.json` один раз после сессии 4. До этого `scripts/tests/test_coverage_metrics.py` (`test_checked_in_baseline_matches_exact_policy_inventory`, `test_validate_baseline_cli_rejects_expired_exception`) красные ожидаемо.
