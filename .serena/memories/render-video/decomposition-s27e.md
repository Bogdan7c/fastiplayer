# render-wgpu-video decomposition — Session 27E (2026-07-12)

## Owner map

- `src/dma_buf_import.rs` + `src/dma_buf_import/safety.rs`: platform/Vulkan DMA-BUF import, fd duplication/ownership, format/feature mapping and plane-view contracts. Safety tests stay beside this owner.
- `src/video/dma_buf_materializer.rs`: neutral provider lookup -> DMA-BUF descriptor validation -> bounded renderer import cache -> typed `WgpuFrameTextureViewLookup`. Public `DmaBufWgpuFrameMaterializer` remains re-exported by `video`.
- `src/video/host_planar_upload.rs`: HostPlanar materializer, reusable texture-pool policy, layout derivation, provider lookup outcomes and generic upload-backend contract.
- `src/video/host_planar_upload/wgpu_backend.rs`: concrete WGPU texture allocation, staging-belt mapped copies, row alignment, batched encoder submit and texture-idle accounting.
- `src/video/nv12_renderer.rs`: coherent NV12 render pipeline and WGSL contract tests.
- `src/video/p010_renderer.rs`: coherent P010 SDR/HDR selection, uniforms, BT.2446-C pipeline binding and adjacent WGSL/Rust contract tests.
- `src/video/host_yuv420_renderer.rs`: coherent HostPlanar 8/high-bit render pipelines, metadata validation/uniform preparation and adjacent WGSL/Rust contract tests.
- `src/color_pipeline.rs` + `src/bt2446c_reference.rs`: color uniform preparation and CPU reference math; shader math is not owned by the materializers.
- `src/resource_provider.rs`: coherent WGPU submission queue binding and exactly-once release/rebind semantics.
- `src/video/mod.rs`: public video facade, common texture-view/result vocabulary, render input/frame validation, renderer dispatch and render orchestration.

## Session 27E decisions

Only two existing mixed source monoliths were split:
1. `video/mod.rs` no longer owns the DMA-BUF materializer/cache implementation.
2. `video/host_planar_upload.rs` no longer owns the concrete WGPU staging backend.

The remaining large renderer/color/resource-provider files were intentionally left coherent: each owns one coupled invariant set, and splitting them by line count would separate WGSL/Rust contract tests or exactly-once release logic from their owner.

This was behavior-neutral. Shader math, texture formats/features, staging/upload accounting, fd ownership, lookup outcomes and the supported frame-contract matrix did not change. Public API paths are preserved through the existing `video` facade re-export.

## Verification

Focused suites: `render-wgpu-video` (99 tests), `render-core`, `capability-core`, `render-wgpu-shell`, filtered `app-egui frame_prepare`, `video-vaapi`, and `video-ffmpeg` passed. Strict Clippy, strict rustdoc, app no-default-features, refactor guardrails, formatting and all-features workspace tests passed. Full pre-PR remains blocked only by tracked dependency advisories in the unchanged lock graph (quick-xml RUSTSEC-2026-0194/0195 and unmaintained audiopus_sys RUSTSEC-2026-0150).