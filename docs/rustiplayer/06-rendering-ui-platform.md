# 06. Rendering, UI и Platform

## Renderer

Production renderer crate: `render-wgpu`.

`render-wgpu` owns:

- WGPU instance/device/surface shell;
- egui composition through `egui-wgpu`;
- NV12 SDR renderer;
- P010 HDR-to-SDR renderer;
- renderer diagnostics and frame timing.

`render-core` owns renderer-neutral contracts and must not create GPU resources.

WGPU boundary в документации описан через surface configuration, texture formats,
adapter/device features и limits. В коде это отражено через `RenderCapabilities`
и WGPU device features.

## Surface formats

Current render input formats:

- `NV12` for SDR 8-bit 4:2:0;
- `P010` for 10-bit 4:2:0 HDR-to-SDR path.

P010 storage layouts:

- `BaselineSeparateLayer`: R16/Rg16 plane views, gated by `TEXTURE_FORMAT_16BIT_NORM`;
- `CompatibilityComposed`: composed P010 path, gated by `TEXTURE_FORMAT_P010`.

Both layouts become the same renderer plane kind after import: Y view plus UV view.

## Color paths

NV12 path:

- shader: `render-wgpu/shaders/nv12_to_rgba.wgsl`;
- output: SDR BT.709;
- defaults preserve current SDR visual result;
- `SwapchainTransferMode::PreserveCurrentUnorm`.

P010 path:

- shader: `render-wgpu/shaders/p010_bt2446c_to_sdr.wgsl`;
- input: strict BT.2020 PQ/HLG P010;
- operator: BT.2446 Method C;
- output: SDR BT.709;
- native HDR output: unsupported.

## UI boundary

UI uses egui and reads `PlayerSnapshot`. Actions become `PlayerCommand`.

`AppState` may own:

- egui context/state;
- shell telemetry;
- startup pending/error overlay;
- cached render-side present frame lease;
- transient timeline pointer state;
- desktop integration handle.

`AppState` must not own:

- demuxer;
- audio decoder/output;
- video decoder thread;
- playback queues;
- capability selection logic;
- codec-specific stream logic.

## Redraw pacing

`app-egui` uses `ControlFlow::Poll` only while playback/opening/scrubbing needs
continuous redraw. Idle and paused states use `Wait` or `WaitUntil` for background
YouTube startup polling.

## Render bridge

`PlayerWorker::try_acquire_present_frame()` returns `PresentFrameLease`.

Render bridge rules:

- render thread reads `PresentFrameLease` resource descriptors and lookup status,
  then asks the active `app-egui`/`render-wgpu` materializer for WGPU texture views;
- lease carries generation, stale flag and decoded frame metadata;
- last safe lease may be reused on transient acquire miss;
- stale generation is rejected;
- release is RAII drop/ack;
- missing views or renderer contract failures become `PlayerRenderError`.

## Desktop integration

`desktop-integration` talks to the player only through public contracts:

- commands: `PlayerCommand`;
- state: `PlayerSnapshot`;
- timeline: `MediaTime`, `MediaDuration`, `TimelineSnapshot`.

Linux MPRIS is platform backend detail. macOS/Windows/stub backends must not leak
platform protocol types into `player-core`.
