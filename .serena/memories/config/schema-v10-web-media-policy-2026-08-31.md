# Config schema v10: web-media policy отделена от yt-dlp (2026-08-31)

## Ownership и публичная форма

- `CURRENT_SCHEMA_VERSION = 10`; актуальный TOML содержит отдельную defaulted strict-секцию `[web_media]`.
- `fastiplayer_config::WebMediaConfig` владеет provider-neutral политикой web media: `hdr_selection`, `preferred_video_height` и всеми пятью `vod_endpoint_recovery_*` значениями.
- `fastiplayer_config::WebMediaHdrSelection` заменяет provider-specific `YtDlpHdrSelection`.
- `YtDlpConfig` теперь содержит только process/extractor controls: `enabled`, timeout и stdout/stderr/JSON limits. Качество и recovery больше не являются частью boundary yt-dlp.
- Settings registry разделён на `web_media_settings.rs` и process-only `yt_dlp_settings.rs`. Stable IDs политики имеют префикс `web_media.*`, route id — `web_media`; labels и descriptions говорят о веб-медиа, а не о качестве yt-dlp.

## Одноразовая migration v9 -> v10

- Raw-TOML migration в `store/migrations.rs` переносит семь policy keys из `[yt_dlp]` в новый `[web_media]`, затем документ получает schema 10.
- Значения пользователя переносятся дословно и затем проходят обычные typed decode + validation.
- Alias-полей в current schema нет. После migration остаётся ровно один source of truth — `[web_media]`.
- Если legacy-документ одновременно содержит target `[web_media]` и старые policy keys в `[yt_dlp]`, migration не пытается угадать приоритет: старые keys остаются и strict `deny_unknown_fields` отклоняет конфликт.
- Current v10 также отклоняет старые policy keys в `[yt_dlp]` и неизвестные поля в `[web_media]`.
- Более старые поддерживаемые версии сначала проходят прежние migration stages, затем ту же v9->v10 boundary migration.

## Runtime routing

- `MediaServiceRuntimeSettingsUpdate` несёт три независимых snapshot: `network`, `web_media`, `yt_dlp`.
- `AppRuntimeRouteGroup::MediaWebMedia` представляет settings group `web_media`.
- `app-egui` передаёт `WebMediaConfig` в stream selection, open-source snapshot и VOD recovery admission; `YtDlpConfig` остаётся рядом только там, где реально нужны process controls.
- Preferred-height apply сохраняет прежний lifecycle contract (rebuild/reopen), recovery policy применяется in-place. Media ingress algorithms в N02 не менялись.

## Инварианты и проверки

- Default document и golden fixture: `crates/config/tests/fixtures/current_schema_v10.toml`.
- Functional config test загружает реальный v9-файл, мигрирует, валидирует, атомарно сохраняет, повторно загружает и проверяет byte/semantic roundtrip и сохранение custom user values.
- Отдельные tests закрепляют conflict/unknown rejection, registry/accessor ownership и provider-neutral labels.
- Settings apply tests проходят draft -> runtime owner -> persistence -> committed snapshot; отдельно проверены preferred-height rebuild и recovery success/rollback.
- Gate N02: config tests, fastiplayer-settings tests, focused app settings/recovery tests, strict affected-package Clippy и workspace `--all-targets --all-features --locked` check.

Related: `mem:config/schema-store-decomposition-s23`, `mem:config/schema-v7-quality-preference-2026-07-21`, `mem:settings-ui/application-contract-s08`.
