# Reduced motion (2026-07-18)

- Публичная additive-настройка: `ui.animations.reduced_motion: bool`, default `true`; schema version не повышалась благодаря `#[serde(default, deny_unknown_fields)]` на вложенной структуре.
- Русское metadata/help описывает настройку как toggle с live Apply route `ui.apply`; старые TOML без поля загружаются со значением `true`, round-trip сохраняет поле.
- `CommittedConfigSnapshot::reduced_motion()` даёт UI подтверждённое значение. `sidebar_slide_duration_seconds()` возвращает `0.0` при reduced motion.
- Playback-rate reveal layout становится мгновенным при reduced motion. Persistent Shuffle/Repeat сохраняют короткие color/opacity transitions, но отключают scale/pulse.
- Default document и `current_schema_v6.toml` явно документируют поле; schema bump не нужен.
- Проверки: config default/legacy/metadata/round-trip, settings Apply routing, committed snapshot, playback-rate и queue-mode motion tests.

- Reusable visibility adapter использует typed `VisibilityMotion::{Standard, Reduced}` из committed config. В режиме Reduced fade и fade-scale мгновенно переходят к target в обоих направлениях; остаточный exit glyph не остаётся. Первый consumer — Playlist header Undo. Контракт и тесты: `mem:animation-core/visibility-2026-07-18`.

Связанные memories: `mem:settings-ui/design`, `mem:config/schema-store-decomposition-s23`, `mem:app-egui/playlist-transport-s18a`, `mem:app-egui/playlist-toolbar-undo-2026-07-18`.


## Общая UI motion policy и active accent (2026-07-19)
- Внутренний `VisibilityMotion` обобщён и переименован в `UiMotion::{Standard, Reduced}`. Это не public config/API change: `CommittedConfigSnapshot::reduced_motion()` по-прежнему является единственным подтверждённым источником настройки.
- Playlist header Undo продолжает использовать тот же мгновенный reduced visibility contract через `UiMotion`. Новый positional active accent получает эту же typed policy через `PlaylistShowInput`, поэтому reduced motion мгновенно переключает nearby rect и необходимый off-screen clamped scroll без residual repaint.
- Standard policy сохраняет Undo 180 ms visibility contract и добавляет Playlist active accent: 220 ms nearby / 360 ms follow. Настройка, schema/default и Apply routing не изменились.
