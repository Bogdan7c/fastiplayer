# Config schema/store decomposition — актуально 2026-08-31

- `crates/config/src/schema.rs` — тонкий `AppConfig` facade/re-export owner map. Section owners: `schema/player.rs`, `video.rs`, `render.rs`, `services.rs` (audio/network/web-media/yt-dlp), `ui.rs`; versions — `schema/version.rs`; generated settings coverage — `schema/metadata_tests.rs`.
- Default TOML documentation принадлежит `schema/default_document.rs`. Комментарии привязаны к serialized field/table; missing target ломает tests, поэтому stale documentation не игнорируется.
- `fastiplayer-config::store` остаётся coordinator API (`load_or_create*`, `load_from_path`, `save_validated_atomic_at`). Atomic persistence — `store/atomic.rs`; legacy normalization/migration — `store/migrations.rs`; defaults/validation/fixtures — `store/tests.rs`.
- Current schema v10. Provider-neutral quality/HDR/VOD recovery policy принадлежит strict/defaulted `[web_media]`; `[yt_dlp]` содержит только process controls. Одноразовая v9->v10 migration переносит policy keys без alias-полей и fail-closed отклоняет конфликт двух секций. Полный contract: `mem:config/schema-v10-web-media-policy-2026-08-31`.
- Прежние migrations `[youtube]` -> `[yt_dlp]` и промежуточные schema upgrades сохранены; старый policy source после v10 не принимается.
- `[ui.sidebar].width_points: u16` default `420`, validation/Settings range `350..=600`, setting id `ui.sidebar.width_points`. Старый документ без section получает default; поле появляется после следующего успешного сохранения.
- Default/min/max sidebar constants экспортируются `fastiplayer-config` и являются единственным источником диапазона для UI/tests. `CommittedConfigSnapshot::sidebar_width_points()` — runtime getter.
- Golden current document: `crates/config/tests/fixtures/current_schema_v10.toml`; byte-stable roundtrip сравнивает его с `AppConfig::default().to_pretty_toml()`.
- `[playlist]` остаётся strict/defaulted group с `load_siblings`, `sibling_media_filter`, playback/error behavior, save debounce, `resume_checkpoint_interval_ms` (default 5000, `1000..=60000`, step 1000) и previous restart threshold. Runtime contracts — `mem:playlist/settings-s13` и `mem:playlist/resume-position-sidecar-2026-07-19`.
- `UiAnimationsConfig::reduced_motion` остаётся defaulted additive field; подробности: `mem:settings-ui/reduced-motion-2026-07-18`.
- Verification owner set: config/settings tests, app settings runtime tests, smoke current config generation+parse, refactor guardrails, locked workspace check, fmt/diff checks.
