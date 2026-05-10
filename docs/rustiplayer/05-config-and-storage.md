# 05. Config and Storage

## Разделение ответственности

Настройки пользователя хранятся в TOML.

Все долговременные данные, кроме настроек, хранятся в SQLite через `rusqlite`.

## Paths

Linux paths:

```text
~/.config/rustiplayer/config.toml
~/.local/share/rustiplayer/rustiplayer.sqlite
~/.cache/rustiplayer/
```

На Windows и macOS эти пути должны проходить через platform path resolver, но логическая структура остается такой же.

## TOML config

TOML выбран как читаемый текстовый формат для широкого спектра настроек.

Rust crate: `toml` + `serde`.

Практический смысл `toml` crate:

- читает `config.toml`;
- через `serde::Deserialize` превращает TOML в Rust structs;
- через `serde::Serialize` может записать default config;
- позволяет версионировать схему.

## Config schema

Минимальная структура:

```toml
schema_version = 1

[player]
start_paused = true
resume_last_position = true
preferred_video_codec_order = ["vp9", "av1", "h264", "h265", "vp8"]

[video]
hardware_decode_only = true
preferred_backend = "auto"
max_decode_ahead_ms = 500
present_queue_frames = 8

[render]
profile = "vulkan"
hdr_to_sdr = false
tone_mapping = "disabled"

[render.color_adjustment]
brightness = 0.0
contrast = 1.0
saturation = 1.0
exposure = 0.0
rgb_gain = [1.0, 1.0, 1.0]
rgb_offset = [0.0, 0.0, 0.0]

[render.vulkan]
present_mode = "fifo"
max_frame_latency = 2

[render.opengles]
enabled = false
simple_ui = true

[audio]
volume = 0.8
output_device = "default"
buffer_target_ms = 200

[network]
cache_enabled = true
max_read_ahead_mb = 256

[youtube]
enabled = true
prefer_account_session = true

[ui]
show_telemetry = true
language = "ru"
```

## Config rules

- Config имеет `schema_version`.
- Defaults живут в коде.
- Отсутствующий config создается из defaults.
- Неизвестные поля на ранних этапах можно логировать как warning.
- Значения проходят validation после deserialization.
- Config не содержит историю, cookies, cache metadata и bookmarks.

## Render color config policy

Phase 8.5 добавляет пользовательские SDR/RGB adjustments в config, но не включает HDR tone mapping в активный renderer path до реализации Phase 10.

В текущей схеме `render.hdr_to_sdr` и `render.tone_mapping` остаются compatibility placeholders:

- default значения: `hdr_to_sdr = false`, `tone_mapping = "disabled"`;
- `app-egui` не пробрасывает эти поля в `ColorPipelineSettings`;
- если пользовательский или старый config включает HDR placeholders, shell логирует warning и продолжает использовать SDR-only capabilities;
- `RenderCapabilities::wgpu_nv12()` всё равно не объявляет HDR/P010 support.

Identity defaults обязательны:

- `brightness = 0.0`;
- `contrast = 1.0`;
- `saturation = 1.0`;
- `exposure = 0.0`;
- `rgb_gain = [1.0, 1.0, 1.0]`;
- `rgb_offset = [0.0, 0.0, 0.0]`.

`swapchain_transfer` и `tone_mapping` сначала живут как typed renderer settings/defaults. Phase 10 не добавляет свободный выбор tone mapping presets в UI: production HDR-to-SDR использует фиксированный `bt2446_c` operator.

## Phase 10 HDR-to-SDR config policy

Текущие scalar placeholders:

```toml
[render]
hdr_to_sdr = false
tone_mapping = "disabled"
```

нельзя одновременно хранить вместе с будущей таблицей:

```toml
[render.hdr_to_sdr]
enabled = true
operator = "bt2446_c"
sdr_reference_white_nits = 100.0
hdr_reference_peak_nits = 1000.0
```

Поэтому Phase 10 должна сделать явную schema migration:

- старый scalar `render.hdr_to_sdr` читается только как compatibility input;
- новый persisted format использует таблицу `[render.hdr_to_sdr]`;
- `render.tone_mapping` остаётся legacy compatibility field или удаляется через schema migration;
- `operator` в Phase 10 принимает только `bt2446_c`;
- UI показывает diagnostics, но не даёт выбрать alternative tone mapping presets.

## Config layering

На первом этапе достаточно user config:

```text
defaults -> ~/.config/rustiplayer/config.toml
```

Позже можно добавить CLI overrides:

```text
defaults -> user config -> CLI override
```

System-level config пока не нужен.

## SQLite storage

Rust crate: `rusqlite`.

Context7 подтвердил базовые практики:

- использовать `Connection`;
- использовать параметризованные queries через `params!`;
- группировать связанные writes в transactions;
- делать migrations через schema version/user_version.

## Storage scope

SQLite хранит:

- history;
- playback progress;
- bookmarks;
- playlists;
- media metadata cache;
- YouTube account/session/cookies;
- service metadata;
- subtitles/captions cache metadata;
- network cache index;
- capability cache;
- crash/error reports;
- telemetry summaries.

## Storage schema modules

Логические модули:

```text
storage/
  migrations/
  connection.rs
  media_library.rs
  playback_history.rs
  bookmarks.rs
  playlists.rs
  service_accounts.rs
  service_sessions.rs
  network_cache.rs
  capability_cache.rs
  error_reports.rs
```

## Migration policy

SQLite schema должна мигрировать вперед.

Правила:

- каждая миграция имеет номер;
- migrations выполняются transactionally;
- failed migration не должна оставлять частично обновленную схему;
- app startup должен ясно сообщать о storage error;
- downgrade не поддерживается на ранних этапах.

## Security note

Account/session/cookies чувствительны.

Базовое решение: хранить в SQLite, как согласовано для всех данных кроме настроек.

Будущий extension point: `CredentialStore`, который сможет использовать OS keyring без изменения `service-youtube` и `player-core`.

```rust
trait CredentialStore {
    fn load_service_session(&self, service: ServiceId) -> anyhow::Result<Option<ServiceSession>>;
    fn save_service_session(&self, session: &ServiceSession) -> anyhow::Result<()>;
}
```

Первичная реализация может быть SQLite-backed.
