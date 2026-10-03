# media-source-open (создан 2026-10-03, session-01 выноса web-media)

Library crate `crates/media-source-open` (lib `media_source_open`): открытие источников без UI. Вынесен из `app-egui` как рефакторинг границы — поведение, тексты ошибок, redaction, generation/identity fences не менялись. План серии: `user/web-media-extraction/` (приватно).

## Модули (объявлены в корне `pub mod` под прежними именами → пути тестов `web_media_quality::tests::...` сохранены)
- `local_media` — открытие локальных файлов (`prepare_local_file`, `open_local_demuxer_from_source`, `LocalDemuxOpenError`, `SUPPORTED_LOCAL_MEDIA_EXTENSIONS`); тестовая TS-фикстура — копия в `local_media/ts_fixture.rs`.
- `direct_progressive_open` — прямые HTTP/FTP (`classify_direct_media_url`, `open_direct_media`, `DirectProgressiveOpenResult`); зависит от `web_media_vod_recovery` и `web_media_demux_registry`.
- `web_media_vod_recovery` (`VodEndpointRecoveryAttachment`), `web_media_demux_registry` (`WebDemuxComposition` — поля `registry`/`capabilities` публичные, как были pub(crate); кандидат на intent-методы отдельной задачей), `web_media_extractor_adapter` (`ExtractorCatalogProjection`/`ExtractorAdapterProjection`), `web_media_hls_subtitles`, `web_media_quality`, `web_media_adaptive_config`.
- Видимость: `pub` только у того, что зовёт app-egui (или стоит в публичной сигнатуре — иначе `private_interfaces`); остальное `pub(crate)`.

## Общие доменные типы web-media (session-02, 2026-10-03)
- `prepared_web_media` — `compose_prepared_web_media` + `PreparedWebMediaAttachments`/`SeekAttachment`/`CompositionError` (единая сборка `PreparedMedia` для всех web adapter-ов). Тесты остались в app (`media_open/preparation/tests.rs`).
- `video_codec_mapping::runtime_video_codec` (config → `codec_core::VideoCodec`, зависимость `codec-core`) + тест.
- `web_media_stream_model` — домен: `WebMediaStreamGeneration`, `WebMediaSelectionPreference`, `WebMediaCandidatePresentation`, `WebMediaStreamConfiguration`, `component_variants/` (+ `component_variants_tests.rs`), 3 доменных теста в `web_media_stream_model/tests.rs`.
- `web_media_catalog` — `model.rs`, `attachment.rs`, `tests.rs`. Внешний вход для coordinator-а — `WebMediaCatalog::from_attachment(generation, parent_generation, &attachment)`; `attachment.choices()/active()` и `WebMediaCatalog::new` остаются внутренними.
- Осталось в app-egui: UI-проекция `web_media_stream_model.rs` (UrlSidebarModel/Controller/Action, 13 UI-тестов в `web_media_stream_model/tests.rs` — пути в evidence S42 не менялись), `web_media_catalog/coordinator.rs` (PlaylistRuntimeBinding). Прежние пути `crate::web_media_stream_model::*`, `crate::web_media_catalog::*`, `crate::media_open::compose_prepared_web_media` сохранены re-export-ами.
- `ItemOverrideState` в sidebar хранит `installed_generation` и сравнивает через `has_same_source_lineage` (UI больше не читает приватное поле generation).
- Fixtures за `cfg(any(test, feature = "test-fixtures"))`: `WebMediaStreamGeneration::for_test`, `WebMediaStreamConfiguration::fixture(parent, candidates, active, preference)` (generation выводится из parent), `selection_for_switch`, `has_video`, `WebMediaSelectionTarget::Fixture`, `WebMediaFacetAction::resolution_for_test`. Нюанс: под `cfg(any(test, feature))` clippy НЕ считает код тестовым → `panic!` в Fixture-ветке требует безусловный `#[expect(clippy::panic, reason)]`. `--all-targets` унифицирует feature в обычную сборку app-egui — production-код app не должен исчерпывающе match-ить `WebMediaSelectionTarget`.
- Evidence: `runtime-coverage-s41.json` пути `compose_prepared_web_media` и `preference_distinguishes_global_default_and_item_override` переведены на media-source-open (решение владельца — править JSON напрямую). Guardrail `PROGRESSIVE_WEB_TRANSIENT_SECRET_SCAN_PATHS` дополнен путями crate-а.

## Находка для session-03
- `web_media_hls_open`/`web_media_dash_open` ещё зависят от `web_media_open::component_variants` (yt-dlp open intents, держится за `YtDlpCandidateOpenIntent` в app) и `web_media_open::catalog_capabilities` (`AppCatalogCapabilityProbe`; требует crate-ы `audio`, `web-media-dash/smooth/hds`). Session-04 переносит их позже, чем session-03 переносит opener-ы — решать порядок/порт осознанно.

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
