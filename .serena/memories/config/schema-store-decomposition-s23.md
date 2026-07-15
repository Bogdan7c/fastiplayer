# Config schema/store decomposition — Session 23 (2026-07-11)

- `crates/config/src/schema.rs` — тонкий стабильный `AppConfig` facade и public re-export owner map. Порядок полей `AppConfig` является порядком current TOML tables и не менялся.
- Section owners: `schema/player.rs`, `video.rs`, `render.rs`, `services.rs` (audio/network/YouTube), `ui.rs`; schema versions — `schema/version.rs`; generated settings coverage — `schema/metadata_tests.rs`.
- Default TOML documentation принадлежит `schema/default_document.rs`. Комментарии ищут актуальный serialized field key/table, а не конкретный default literal; отсутствующий target немедленно ломает tests. Поэтому изменённые пользовательские значения сохраняют комментарии, а stale documentation не игнорируется молча.
- `rustiplayer-config::store` остаётся публичным coordinator API (`load_or_create*`, `load_from_path`, `save_validated_atomic_at`). Atomic persistence принадлежит `store/atomic.rs`; legacy v2/v3/v4 normalization — `store/migrations.rs`; integration defaults/validation/fixtures вынесены из coordinator в `store/tests.rs`.
- Schema остаётся v5. Strict `deny_unknown_fields`, TOML names/order/defaults и v2/v3/v4 migration semantics сохранены. Отсутствующий `youtube.hdr_selection` в v5 по-прежнему даёт `SdrOnly`; version bump не выполнялся.
- Golden current-schema document: `crates/config/tests/fixtures/current_schema_v5.toml`. Focused test сравнивает его byte-for-byte с `AppConfig::default().to_pretty_toml()`, парсит и повторно сериализует.
- Проверки Session 23: config/settings tests, app settings runtime tests, smoke current config generation+parse, refactor guardrails, `cargo check --workspace --locked`, fmt/diff checks.

## Playlist config — Session 13 (2026-07-15)
- `AppConfig` имеет additive/defaulted strict group `[playlist]`; current schema остаётся v5. Legacy v5 без section загружается с `PlaylistConfig::default()` и не переписывается при startup.
- `PlaylistConfig` владеет `load_siblings`, `sibling_media_filter`, default `playback_behavior`, `error_behavior`, `state_save_debounce_ms` (250..=30_000) и `previous_restart_threshold_ms` (0..=60_000), stable snake_case enum ids и русскими generated descriptors.
- Default TOML/fixture и strict registry coverage включают все шесть leaf fields. Подробный runtime contract — `mem:playlist/settings-s13`.
