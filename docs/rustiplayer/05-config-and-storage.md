# 05. Config и Runtime Data

## Ответственность

`crates/config` owns user configuration only:

- TOML schema;
- defaults;
- validation;
- platform config path;
- load/create store helpers.

It does not own playback state, UI state, cookies, history, bookmarks, cache
metadata или service sessions.

## Пути

Linux config file:

```text
~/.config/rustiplayer/config.toml
```

`directories` is used for platform paths. Cache directories may exist, but durable
cache metadata is not part of the current architecture.

## Схема

Current schema version: `2`.

Top-level sections:

```toml
schema_version = 2

[player]
[player.seek]
[player.demux]
[video]
[video.scheduler]
[render]
[render.hdr_to_sdr]
[render.color_adjustment]
[render.vulkan]
[render.opengles]
[audio]
[network]
[youtube]
[ui]
```

Unknown fields are rejected with `deny_unknown_fields`. Values are validated after
Serde deserialization.

## Важные defaults

- `player.start_paused = true`
- `player.resume_last_position = true`
- `player.demux.max_consecutive_corrupted_packets = 64`
- `video.hardware_decode_only = true`
- `video.present_queue_frames = 8`
- `video.decoder_packet_channel_frames = 32`
- `video.decoder_frame_channel_frames = 8`
- `video.decoder_ready_queue_frames = 8`
- `video.decoder_surface_pool_frames = 24`
- `video.zero_copy_surface_pool_slots = 24`
- `video.scheduler.decode_ahead_target_ms = 250`
- `render.profile = "vulkan"`
- `render.hdr_to_sdr.enabled = true`
- `render.hdr_to_sdr.operator = "bt2446_c"`
- `render.vulkan.present_mode = "fifo"`
- `render.vulkan.max_frame_latency = 2`
- `network.memory_cache_mb = 128`
- `network.read_ahead_mb = 256`
- `network.prefetch_initial_chunk_kb = 64`
- `network.prefetch_chunk_mb = 8`
- `youtube.enabled = true`
- `youtube.prefer_account_session = true`
- `youtube.resolve_timeout_ms = 30000`
- `ui.skin = "minimal"`

## Инварианты вне config

These are not user switches:

- zero-copy video only;
- no software video fallback;
- no CPU upload/readback fallback;
- native HDR output disabled;
- only BT.2446-C HDR-to-SDR operator in current production path.

If tests need CPU-visible helpers, they must be compile-time test paths, not TOML,
env or UI toggles.

## Runtime-only data

Runtime state lives in memory:

- `PlayerWorker` command/event/snapshot channels;
- `PlayerSession` playback state and queues;
- `source-core` RAM byte-range cache;
- decoder texture pool and render leases;
- telemetry and diagnostics.

The previous idea of a durable database/index layer is not current architecture.
No `rusqlite` crate is present in the workspace.

## Compatibility notes

`render.hdr_to_sdr` supports reading the old scalar placeholder and maps it to the
current table defaults. New persisted defaults should use `[render.hdr_to_sdr]`.

`render.tone_mapping` remains a legacy/future config field. It does not expose
multiple production tone-mapping presets today.
