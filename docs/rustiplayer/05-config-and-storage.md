# 05. Config and Runtime Data

## Разделение ответственности

Настройки пользователя хранятся в TOML.

Долговременный database слой удалён из текущей архитектуры. Durable seek/cache
metadata не сохраняются, а runtime keyframe/time index слоя больше нет, чтобы
дополнительный IO не попадал в playback, seek или scrub path.

## Paths

Linux paths:

```text
~/.config/rustiplayer/config.toml
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

Актуальная config schema version: `2`.

Минимальная структура:

```toml
schema_version = 2

[player]
start_paused = true
resume_last_position = true
preferred_video_codec_order = ["vp9", "av1", "h264", "h265", "vp8"]

[player.seek]
live_interval_ms = 100
live_preview_budget_ms = 100
commit_timeout_ms = 10000
resume_audio_min_buffer_ms = 50
resume_video_min_ready_frames = 3
paused_commit_behavior = "stay_paused"
hotkey_small_step_secs = 5
hotkey_large_step_secs = 30

[video]
hardware_decode_only = true
preferred_backend = "auto"
max_decode_ahead_ms = 500
present_queue_frames = 8
decoder_packet_channel_frames = 32
decoder_frame_channel_frames = 8
decoder_ready_queue_frames = 8
decoder_surface_pool_frames = 24
zero_copy_surface_pool_slots = 24

[video.scheduler]
demux_packets_per_tick = 12
video_packets_per_tick = 8
decoded_frames_per_tick = 8
catch_up_budget_ms = 4
present_queue_min_frames = 2
present_queue_target_frames = 4
decode_ahead_target_ms = 250
surface_free_slots_min = 2
surface_free_slots_target = 4

[render]
profile = "vulkan"
tone_mapping = "disabled"

[render.hdr_to_sdr]
enabled = true
operator = "bt2446_c"
sdr_reference_white_nits = 100.0
hdr_reference_peak_nits = 1000.0

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
memory_cache_mb = 128
read_ahead_mb = 64
connect_timeout_ms = 15000
read_timeout_ms = 15000

[youtube]
enabled = true
prefer_account_session = true

[ui]
show_telemetry = true
language = "ru"
skin = "minimal"
```

## Config rules

- Config имеет `schema_version`.
- Defaults живут в коде.
- Отсутствующий config создается из defaults.
- Неизвестные поля являются ошибкой TOML-схемы.
- Значения проходят validation после deserialization.
- Config не содержит историю, cookies, cache metadata и bookmarks.

## Video zero-copy policy

`zero_copy_video_only = true` является архитектурным инвариантом, а не
пользовательской настройкой TOML. Production video path принимает только
hardware decode + DMA-BUF zero-copy export/import; CPU upload, CPU readback и
software video fallback не имеют runtime-переключателя.

Если для regression tests понадобится CPU-visible helper, он должен быть
отдельным compile-time test-only path. Такой helper нельзя подключать через env,
config, diagnostic mode или UI.

## Schema version 2 seek/network/UI policy

Schema version 2 фиксирует публичные knobs для live seek/scrub,
source-cache слоя и selectable UI skin. Эти поля не должны превращаться в
магические константы в `player-core`, `source-core` или `app-egui`.

`player.seek.*`:

- `live_interval_ms = 100` - минимальный интервал live scrub update-ов;
- `live_preview_budget_ms = 100` - budget preview work на один update;
- `commit_timeout_ms = 10000` - typed timeout финального commit-а;
- `resume_audio_min_buffer_ms = 50` - минимальный audio buffer перед resume;
- `resume_video_min_ready_frames = 3` - минимальный запас готовых video frames перед resume;
- `paused_commit_behavior = "stay_paused"` - seek из паузы остаётся на паузе;
- `hotkey_small_step_secs = 5` - малый relative seek step;
- `hotkey_large_step_secs = 30` - большой relative seek step.

`network.*`:

- `memory_cache_mb = 128` - RAM cache budget; `0` явно отключает RAM cache;
- `read_ahead_mb = 64` - сетевой read-ahead budget;
- `connect_timeout_ms = 15000` - timeout подключения;
- `read_timeout_ms = 15000` - timeout чтения;

`ui.skin = "minimal"` - единственный skin, который текущая validation принимает
без mapping. Неизвестный skin id является config error; silent fallback запрещён,
пока validation явно не описывает такой mapping.

Validation rules:

- `network.memory_cache_mb <= 4096`; ноль валиден и отключает RAM cache;
- network timeouts положительные;
- seek intervals, budgets, timeout, resume video preroll и hotkey steps положительные;
- unknown `ui.skin` rejected как config error.

## Render color config policy

Phase 8.5 добавил пользовательские SDR/RGB adjustments в config. Phase 10 добавил активную `[render.hdr_to_sdr]` таблицу для production HDR-to-SDR path.

Текущая схема:

- `[render.hdr_to_sdr]` активна и по умолчанию включает `bt2446_c` с `100.0` SDR reference white и `1000.0` HDR reference peak;
- `app-egui` пробрасывает HDR-to-SDR settings в renderer boundary через typed settings, но не содержит color math;
- `render.tone_mapping = "disabled"` остаётся legacy/future field и не открывает пользовательские tone mapping presets;
- renderer capabilities объявляют HDR-to-SDR только когда P010 renderable path и BT.2446-C реально доступны;
- SDR/NV12 path не зависит от HDR shader и продолжает использовать identity SDR adjustments по умолчанию.

Identity defaults обязательны:

- `brightness = 0.0`;
- `contrast = 1.0`;
- `saturation = 1.0`;
- `exposure = 0.0`;
- `rgb_gain = [1.0, 1.0, 1.0]`;
- `rgb_offset = [0.0, 0.0, 0.0]`.

`swapchain_transfer` и `tone_mapping` живут как typed renderer settings/defaults. Phase 10 не добавляет свободный выбор tone mapping presets в UI: production HDR-to-SDR использует фиксированный `bt2446_c` operator.

## Phase 10 HDR-to-SDR config policy

Старый scalar placeholder из Phase 8.5:

```toml
[render]
hdr_to_sdr = false
tone_mapping = "disabled"
```

нельзя одновременно хранить вместе с текущей таблицей:

```toml
[render.hdr_to_sdr]
enabled = true
operator = "bt2446_c"
sdr_reference_white_nits = 100.0
hdr_reference_peak_nits = 1000.0
```

Phase 10 реализует совместимый read-path и новый persisted/default format:

- старый scalar `render.hdr_to_sdr` читается только как compatibility input;
- новый persisted format использует таблицу `[render.hdr_to_sdr]`;
- `render.tone_mapping` остаётся legacy/future field, но не заменяет `operator`;
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

## Runtime-only data

В текущем коде нет database crate, долговременного cache хранилища и runtime
`BackgroundIndexer`.

Runtime-only остаются:

- telemetry counters;
- текущие `PlayerSnapshot`/`PlayerWorkerEvent`;
- временные service descriptors, полученные при открытии media.

Правила:

- `app-egui` не открывает database connection;
- `player-core` не строит runtime keyframe/time index;
- `source-core` не строит local partial hash для durable identity;
- legacy index-only поля `network.index_fingerprint_sample_kb` и
  `network.indexer_io_budget_mb_per_sec` считаются unknown fields и не
  принимаются schema validation.

## Security note

Account/session/cookies чувствительны.

Текущее решение: не хранить account/session/cookies в проектном database слое.

Будущий extension point: `CredentialStore`, который сможет использовать OS keyring без изменения `service-youtube` и `player-core`.

```rust
trait CredentialStore {
    fn load_service_session(&self, service: ServiceId) -> anyhow::Result<Option<ServiceSession>>;
    fn save_service_session(&self, session: &ServiceSession) -> anyhow::Result<()>;
}
```
