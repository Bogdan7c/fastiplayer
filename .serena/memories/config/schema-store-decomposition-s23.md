# Config schema/store decomposition — актуально 2026-07-17

- `crates/config/src/schema.rs` — тонкий `AppConfig` facade/re-export owner map. Section owners: `schema/player.rs`, `video.rs`, `render.rs`, `services.rs` (audio/network/yt-dlp), `ui.rs`; versions — `schema/version.rs`; generated settings coverage — `schema/metadata_tests.rs`.
- Default TOML documentation принадлежит `schema/default_document.rs`. Комментарии привязаны к serialized field/table; missing target ломает tests, поэтому stale documentation не игнорируется.
- `rustiplayer-config::store` остаётся coordinator API (`load_or_create*`, `load_from_path`, `save_validated_atomic_at`). Atomic persistence — `store/atomic.rs`; legacy normalization/migration — `store/migrations.rs`; defaults/validation/fixtures — `store/tests.rs`.
- Current schema v7. Legacy v2-v5 `[youtube]` мигрируется в `[yt_dlp]`, v6 получает default global quality preference, placeholder `prefer_account_session` удаляется. Current v7 строго отвергает old section/placeholder и runtime-only item override keys. Полный contract: `mem:config/schema-v7-quality-preference-2026-07-21`.
- `[ui.sidebar].width_points: u16` добавлен backward-compatible без schema bump: serde default `420`, validation/Settings range `350..=600`, setting id `ui.sidebar.width_points`. Старый v6 без section загружается с default; поле появляется после следующего успешного сохранения.
- Default/min/max sidebar constants экспортируются `rustiplayer-config` и являются единственным источником диапазона для UI/tests. `CommittedConfigSnapshot::sidebar_width_points()` — runtime getter.
- Golden current document: `crates/config/tests/fixtures/current_schema_v7.toml`; byte-stable roundtrip сравнивает его с `AppConfig::default().to_pretty_toml()`.
- `[playlist]` остаётся strict/defaulted group с `load_siblings`, `sibling_media_filter`, playback/error behavior, save debounce, `resume_checkpoint_interval_ms` (default 5000, `1000..=60000`, step 1000) и previous restart threshold. `player.resume_last_position` теперь активирует sidecar capture/startup restore. Оба изменения backward-compatible и не повышают schema v6. Legacy documents без section получают `PlaylistConfig::default()` без обязательной startup rewrite; runtime contracts — `mem:playlist/settings-s13` и `mem:playlist/resume-position-sidecar-2026-07-19`.
- Verification owner set: config/settings tests, app settings runtime tests, smoke current config generation+parse, refactor guardrails, locked workspace check, fmt/diff checks.

## Additive UI animation field (2026-07-18)

`UiAnimationsConfig` теперь содержит defaulted `reduced_motion: bool` со значением `true`. Поле добавлено в default document/current schema fixture и settings metadata; старый TOML совместим, поэтому `CURRENT_SCHEMA_VERSION` не повышался. Подробности UI-семантики: `mem:settings-ui/reduced-motion-2026-07-18`.
