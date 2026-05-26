# Render Video

- `video-core` owns decoded frame, opaque texture handle, and video diagnostics contracts. Production decoded formats are `Nv12` and `P010`; production memory path is `DmaBufZeroCopy`.
- `video-core::VideoDecoderThreadHandle` exposes a renderer/resource provider associated type, not concrete GPU handles. `player-core` specializes it to `PresentFrameResourceProviderHandle` for neutral lookup/release accounting.
- `video-vaapi` owns VA-API probe/decode thread, backend queues, DMA-BUF export/import, GBM/dma-heap/frame pool, texture cache, and WGPU import adapter.
- `video-vaapi::VaapiWgpuVideoBackendFactory` is now an app composition startup helper, not a `player-core` factory. `start_for_composition()` returns both `StartedVideoBackend` for playback and `VideoTextureViewProvider` for WGPU materialization.
- `video-vaapi` implements `player-core::PresentFrameResourceProvider` for `VideoTextureViewProvider` only to provide renderer-neutral Ready/Busy/Missing/Error status, lock diagnostics, and release path. The WGPU views themselves stay out of `player-core`.
- `render-core` owns renderer-neutral capabilities/color/render diagnostics and must not allocate GPU resources.
- `render-wgpu` owns WGPU instance/device/surface shell, egui composition, NV12 renderer, P010 HDR-to-SDR renderer, diagnostics, frame timing, and WGPU materialization API (`WgpuFrameTextureViewMaterializer`, `WgpuFrameTextureViewLookup`, `WgpuFrameTextureViews`).
- `render-wgpu` source modules include `capabilities`, `color_pipeline`, `egui_compositor`, `frame`, `shell`, `video`, `bt2446c_reference`; shaders live under `crates/render-wgpu/shaders`.
- `WgpuRenderableFrame` constructors validate decoded frame metadata. NV12 and P010 paths reject non-zero-copy memory paths; plane/metadata mismatch is a render boundary error.
- P010 support depends on WGPU device features/layout: separate R16/Rg16 plane views or composed P010 path. Both become Y plus UV plane views at renderer boundary.
- SDR path shader: `nv12_to_rgba.wgsl`; HDR path shader: `p010_bt2446c_to_sdr.wgsl`; HDR output is SDR BT.709 via BT.2446 Method C.
- Render bridge uses `PlayerWorker::try_acquire_present_frame()` -> `PresentFrameLease`; `app-egui` asks the active `render-wgpu`/backend materializer for WGPU plane views by opaque frame handle, reports lookup lock diagnostics back through the lease, and keeps Ready/Busy/Missing/Error distinct.
- RAII drop/ack on `PresentFrameLease` still releases texture/resource slots through the provider that created the frame, including stale-generation cases.
- If render drops its lease before player-core releases the decoded frame, the original provider must survive until player-owned release so VA-API can use the GPU-submission-aware release path instead of immediate decoder release.
- Busy materialization may reuse the previous cached renderable frame. Missing/Error clear the cached renderable frame and become typed `PlayerRenderError`; silent black-frame fallback is only acceptable before a frame exists.
- Do not add CPU upload/readback fallback or move VA-API decode pool ownership into render/app layers without an explicit architecture decision.