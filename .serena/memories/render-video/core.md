# Render Video

- `video-core` owns decoded frame, opaque texture handle, and video diagnostics contracts. Production decoded formats are `Nv12` and `P010`; production memory path is `DmaBufZeroCopy`.
- `video-vaapi` owns VA-API probe/decode thread, backend queues, DMA-BUF export/import, GBM/dma-heap/frame pool, texture cache, and WGPU import adapter.
- `video-vaapi::VaapiWgpuVideoBackendFactory` implements `player-core::VideoBackendFactory`; this `video-vaapi -> player-core` dependency is current adapter debt, not a model for decoder internals reading session/pipeline state.
- `render-core` owns renderer-neutral capabilities/color/render diagnostics and must not allocate GPU resources.
- `render-wgpu` owns WGPU instance/device/surface shell, egui composition, NV12 renderer, P010 HDR-to-SDR renderer, diagnostics, and frame timing.
- `render-wgpu` source modules include `capabilities`, `color_pipeline`, `egui_compositor`, `frame`, `shell`, `video`, `bt2446c_reference`; shaders live under `crates/render-wgpu/shaders`.
- `WgpuRenderableFrame` constructors validate decoded frame metadata. NV12 and P010 paths reject non-zero-copy memory paths; plane/metadata mismatch is a render boundary error.
- P010 support depends on WGPU device features/layout: separate R16/Rg16 plane views or composed P010 path. Both become Y plus UV plane views at renderer boundary.
- SDR path shader: `nv12_to_rgba.wgsl`; HDR path shader: `p010_bt2446c_to_sdr.wgsl`; HDR output is SDR BT.709 via BT.2446 Method C.
- Render bridge uses `PlayerWorker::try_acquire_present_frame()` -> `PresentFrameLease`; texture views are created on render thread; stale generation rejected; RAII drop/ack releases texture slots.
- Silent black-frame fallback is only acceptable before a frame exists. Render contract violations should become typed `PlayerRenderError`.