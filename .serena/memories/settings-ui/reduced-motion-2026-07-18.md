# Reduced motion (2026-07-18)

- Публичная additive-настройка: `ui.animations.reduced_motion: bool`, default `true`; schema version не повышалась благодаря `#[serde(default, deny_unknown_fields)]` на вложенной структуре.
- Русское metadata/help описывает настройку как toggle с live Apply route `ui.apply`; старые TOML без поля загружаются со значением `true`, round-trip сохраняет поле.
- `CommittedConfigSnapshot::reduced_motion()` даёт UI подтверждённое значение. `sidebar_slide_duration_seconds()` возвращает `0.0` при reduced motion.
- Playback-rate reveal layout становится мгновенным при reduced motion. Persistent Shuffle/Repeat сохраняют короткие color/opacity transitions, но отключают scale/pulse.
- Default document и `current_schema_v6.toml` явно документируют поле; schema bump не нужен.
- Проверки: config default/legacy/metadata/round-trip, settings Apply routing, committed snapshot, playback-rate и queue-mode motion tests.

Связанные memories: `mem:settings-ui/design`, `mem:config/schema-store-decomposition-s23`, `mem:app-egui/playlist-transport-s18a`.
