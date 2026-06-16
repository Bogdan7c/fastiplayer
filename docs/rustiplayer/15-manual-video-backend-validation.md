# 15. Manual Video Backend Validation

Эти проверки нужны для release-only video backend validation. Debug/dev build не
является доказательством playback качества: оценивать backend selection,
decode/render pacing и FFmpeg software path нужно через `cargo run --release`.

## Общие правила

- Перед запуском сохранить текущий `~/.config/rustiplayer/config.toml`, если
  меняешь `video.preferred_backend` вручную.
- Включить diagnostics, чтобы видеть selected app plan, capability report,
  pixel layout, transfer path и FFmpeg probe failure:

```bash
RUST_LOG=info,app_egui=debug,player_core=debug,capability_core=debug,video_ffmpeg=debug \
cargo run --release -p app-egui -- /path/to/media
```

- В capability report искать строки вида `backend ...`, `pixel layout: ...`,
  `transfer path: ...`.
- В startup logs искать `Selected video pipeline` с `plan=vaapi-dmabuf-wgpu`
  или `plan=ffmpeg-host-upload-wgpu`.
- Любой fallback должен быть следствием `video.preferred_backend = "auto"` и
  renderer-intersected `SystemCapabilities::playable_video_outputs`, а не
  silent retry после runtime failure.

## Сценарии

### Hardware-playable media

Config:

```toml
[video]
preferred_backend = "auto"
```

Expected:

- selected plan: `vaapi-dmabuf-wgpu`;
- capability output backend: `vaapi`;
- pixel layout: `NV12` for SDR 8-bit 4:2:0 or `P010` for HDR 10-bit 4:2:0;
- transfer path: `hardware zero-copy via DMA-BUF`;
- no FFmpeg startup is required for playback.

### Software-only media

Use media whose codec/profile/layout is not playable by current VA-API policy
but is present in the FFmpeg software matrix.

Config:

```toml
[video]
preferred_backend = "auto"
```

Expected:

- selected plan: `ffmpeg-host-upload-wgpu`;
- capability output backend: `ffmpeg-sw`;
- pixel layout: one of the explicit HostPlanar YUV layouts;
- transfer path: `software host upload`;
- renderer does one host-to-GPU upload and keeps YUV/color/HDR processing on GPU.

### Missing FFmpeg Runtime

Run without `LD_LIBRARY_PATH`/system runtime for FFmpeg 8.1.x, or use the default
workspace build without feature `ffmpeg`.

Expected:

- app starts;
- hardware-playable media still uses VA-API when available;
- FFmpeg software backend report is unavailable with one of:
  `no-build`, `missing-runtime-libs`, `too-old`, `probe-failed`;
- `video.preferred_backend = "software"` reports a typed unavailable selection
  error instead of starting VA-API.

### `preferred_backend = "hardware"`

Expected:

- hardware-playable media selects `vaapi-dmabuf-wgpu`;
- software-only media is rejected;
- no fallback to `ffmpeg-host-upload-wgpu`.

### `preferred_backend = "software"`

Expected:

- playable FFmpeg media selects `ffmpeg-host-upload-wgpu`;
- VA-API is not started as a fallback;
- missing/unavailable FFmpeg runtime returns a typed unavailable error.

## Guardrails While Validating

- Do not enable FFmpeg hardware acceleration or hwaccel flags.
- Do not add `ffmpeg_sw`/`ffmpeg-sw` TOML keys; use
  `video.preferred_backend = "software"`.
- Do not use swscale/libswscale for playback conversion.
- Do not judge playback smoothness from a dev build.
