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

Current schema version: `4`.

Top-level sections:

```toml
schema_version = 4

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

`video.preferred_backend` accepts only `"auto"`, `"hardware"` and `"software"`.
Legacy schema v2 `"vaapi"` is loaded as `"hardware"` in memory. The old
`"vulkan"` video decode preference remains removed and is rejected with a
suggested fix instead of being migrated or silently mapped to a current backend.
Schema v4 keeps decode-path selection only in `video.preferred_backend`.

`"hardware"` means the native hardware decode path; on Linux today that concrete
path is VA-API, but the public config value is intentionally not VA-API-specific.
`"software"` means FFmpeg software decode only; if the FFmpeg software provider
is unavailable, selection fails with a typed error.
`"auto"` prefers playable native hardware outputs and falls back to FFmpeg
software only after capability selection proves that no supported hardware plan
is playable and a renderer-intersected software output exists. There is no
separate `ffmpeg_sw` or `ffmpeg-sw` config key.

`render.profile = "vulkan"` and `[render.vulkan]` are render/surface settings
for the current WGPU shell path. They do not select a Vulkan video decode
backend.

## Важные defaults

- `player.start_paused = true`
- `player.resume_last_position = true`
- `player.demux.max_consecutive_corrupted_packets = 64`
- `video.preferred_backend = "auto"`
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

- FFmpeg hardware decode;
- CPU RGB conversion or swscale playback conversion;
- CPU readback fallback;
- native HDR output disabled;
- only BT.2446-C HDR-to-SDR operator in current production path.

Software decode is selected only through `video.preferred_backend = "auto"` or
`"software"` and only when FFmpeg runtime probing plus renderer capability
intersection succeeds.

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

Schema v2 `video.preferred_backend = "vaapi"` is accepted as a compatibility
value and loaded as `"hardware"` in memory. The removed `"vulkan"` decode value
is rejected with a targeted error and should be changed to `"auto"` or
`"hardware"`.

`render.tone_mapping` remains a legacy/future config field. It does not expose
multiple production tone-mapping presets today.
