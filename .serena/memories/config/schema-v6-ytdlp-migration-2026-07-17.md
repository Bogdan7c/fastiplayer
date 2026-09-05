# Config schema v6: generic yt-dlp service (2026-07-17)

> Historical v6 migration contract. Current schema is v7; see `mem:config/schema-v7-quality-preference-2026-07-21`.

- `CURRENT_SCHEMA_VERSION = 6`; поддерживаемые legacy versions остаются v2-v5.
- Current typed owner: `fastiplayer_config::YtDlpConfig` в `AppConfig::yt_dlp`, с `enabled`, `hdr_selection: YtDlpHdrSelection` и `resolve_timeout_ms`. Stable section/setting IDs: `[yt_dlp]`, `yt_dlp.enabled`, `yt_dlp.hdr_selection`, `yt_dlp.resolve_timeout_ms`.
- До strict Serde parse migration для schema v2-v5 переименовывает table `[youtube]` в `[yt_dlp]`, сохраняет `enabled`, HDR policy и timeout, удаляет legacy placeholder `prefer_account_session`, затем поднимает schema_version до 6.
- Migration не merge-ит одновременно существующие `[youtube]` и `[yt_dlp]`: ambiguous/unknown shape должен fail closed на strict parse, а не угадывать приоритет.
- В current schema v6 `[youtube]` и `prefer_account_session` строго запрещены. Поле session удалено без изменения реального поведения: системный `yt-dlp` как и раньше читает собственные config/cookies; app credential/auth config не добавлен.
- Default document, golden fixture, smoke config helper, settings metadata/application contract и playback-smoke assertions используют v6/[yt_dlp]. Current TOML roundtrip текстово стабилен.
- Focused tests: `crates/config/src/store/migrations.rs`, `crates/config/src/store/tests.rs`, `crates/config/src/schema/version.rs`, `crates/config/tests/fixtures/current_schema_v6.toml`, `scripts/tests/playback-smoke-self-test.sh`.
- S26 runtime note (2026-07-22): system yt-dlp effective headers/cookies теперь проходят только через ephemeral transport context/jar; app credential/browser/profile config по-прежнему отсутствует, current v7 продолжает строго отвергать removed placeholder. См. `mem:media-services/ytdlp-system-auth-s26-2026-07-22`.