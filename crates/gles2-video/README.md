# gles2-video

A video layer for OpenGL ES 2.0, built on [`gles2-egui`](../gles2-egui/README.md). It draws
planar YUV frames under an egui interface with color conversion, letterboxing, rotation,
color adjustment, HDR-to-SDR tone mapping and bitmap overlays (subtitles, titles). Fastiplayer
plays all of its video through it on hardware without Vulkan, and it is written so that any
egui application, such as a video editor, can use it.

It is part of the [Fastiplayer](../../README.md) workspace and is not published on crates.io yet:
use it as a git or path dependency. The API may still change.

## What it gives you

- **Planar YUV from CPU memory with exactly one copy.** 8-bit 4:2:0, 4:2:2 and 4:4:4; 10- and
  12-bit 4:2:0/4:2:2 and 10-bit 4:4:4 (little-endian 16-bit samples). The driver copies rows
  straight into textures: no CPU repacking, and without `GL_EXT_unpack_subimage` the texture is
  as wide as the stride and the shader crops the tail.
- **Zero-copy import of DMA-BUF frames.** Hardware-decoded NV12 (for example VA-API surfaces)
  through `import_dma_buf_frame`, and planar frames in kernel buffers through
  `import_planar_dma_buf_frame` (Fastiplayer uses this for software-decoded frames: decoder
  memory in a sealed memfd, exported with `/dev/udmabuf`). Imports are cached by buffer identity
  with a bounded cache. `probe_cpu_write_visibility` tells you whether the GPU sees CPU writes to
  such buffers without a cache flush.
- **A bounded upload ring** (`UploadSlotCount`, 3 slots by default). A slot is only rewritten
  after the GPU fence of the frame that read it; with no free slot `upload` returns
  `UploadOutcome::Busy` and the previous frame stays on screen. Nothing grows without a limit.
- **Color the way Fastiplayer's Vulkan renderer does it:** BT.601/BT.709, limited/full range,
  brightness, contrast, saturation, exposure, per-channel gain and offset, quarter-turn rotation,
  letterbox, and visible-rectangle lists for parts covered by side panels. Pixel parity with the
  wgpu renderer is tested in the workspace (max difference 1/255 for SDR, 2/255 for HDR).
- **HDR-to-SDR BT.2446-C** for 10/12-bit PQ and HLG frames. The formula is evaluated in `f64` on
  the CPU at the nodes of a 65³ table, and the shader blends the nodes; this keeps the GPU cost
  inside a frame budget on a 2010 iGPU. Native HDR output is not a goal: the result is SDR BT.709.
- **Bitmap overlays** (`BitmapOverlayRenderer`): coverage masks with a color, and premultiplied
  RGBA pictures, packed into atlases.

The only workspace dependency is `gles2-egui`; the crate does not know any Fastiplayer player
crate (a guardrail script enforces this). Its frame vocabulary (`PlanarFrameUpload`,
`PlaneSource`) is its own; Fastiplayer translates decoder frames into it in a separate adapter
crate.

## Minimal use

[`examples/egui_video_window.rs`](examples/egui_video_window.rs) is a complete program: a winit
window, egui input through `egui-winit`, a CPU-generated YUV 4:2:0 picture, a brightness slider
and a pause checkbox. Run it with:

```sh
cargo run --release -p gles2-video --example egui_video_window
cargo run --release -p gles2-video --example egui_video_window -- --scripted-check
```

`--scripted-check` shows 120 frames without user input and checks that every presented frame
contained video and that new frames were actually uploaded; it prints `scripted check: PASS`.
CI compiles the example on every push.

One frame, in order:

```rust
use std::sync::Arc;
use gles2_video::{
    ChromaSubsampling, ColorAdjustment, HostRows, PictureMapping, PixelRect, PlanarFrameUpload,
    PlanarLayout, PlaneExtent, PlaneSource, QuarterTurn, SampleDepth, UploadOutcome,
    VideoColorConversion, VideoDrawRequest, VideoFrameKey, VideoLayerOptions,
    VideoLayerRenderer, VideoUnderlay, YuvColorEncoding,
};

// Once, next to the gles2-egui context and compositor.
let mut video = VideoLayerRenderer::new(&context, VideoLayerOptions::default())?;

// 1. Upload a new frame BEFORE the compositor starts the frame. A new key = a new picture;
//    the same key again is `AlreadyCurrent` and copies nothing.
let frame = PlanarFrameUpload {
    key: VideoFrameKey(frame_number),
    layout: PlanarLayout { chroma: ChromaSubsampling::Yuv420, depth: SampleDepth::Bits8 },
    luma_extent: PlaneExtent { width, height },
    luma: PlaneSource::HostRows(HostRows::new(&y_plane, y_stride, width as usize, height)?),
    chroma_u: PlaneSource::HostRows(HostRows::new(&u_plane, uv_stride, uv_width, uv_height)?),
    chroma_v: PlaneSource::HostRows(HostRows::new(&v_plane, uv_stride, uv_width, uv_height)?),
};
match video.upload(&context, &frame)? {
    UploadOutcome::Uploaded(_) | UploadOutcome::AlreadyCurrent => {}
    UploadOutcome::Busy(_) => { /* GPU still reads every slot: keep the previous picture */ }
}

// 2. Draw it as the underlay of the egui frame.
let viewport = PixelRect { x: 0, y: 0, width: target_width, height: target_height };
let mut underlay = VideoUnderlay::new(&mut video, VideoDrawRequest {
    viewport,
    visible_rects: &[viewport],
    picture: PictureMapping::FILL, // or a letterbox mapping, see the example
    orientation: QuarterTurn::Identity,
    color: VideoColorConversion::Sdr {
        encoding: YuvColorEncoding::SDR_BT709_LIMITED,
        adjustment: ColorAdjustment::IDENTITY,
    },
});
// ... `compositor.render(&mut context, UiFrameRequest { layers: &mut underlay, .. })?`

// 3. Fence the frame's GPU work, so ring slots (and your decoder buffers) are released only
//    after the GPU has read them. Without this, later uploads answer `Busy`.
let fence = Arc::new(context.insert_fence()?);
video.retire_submitted_work(Arc::clone(&fence));
// The same `Arc<GpuFence>` can tell your decoder when its buffer may be reused.
```

Host rows are copied during `upload`, so a CPU buffer can be reused as soon as `upload` returns.
Imported DMA-BUF frames are read by the GPU later: give those buffers back only after the fence
of the last frame that drew them.

## Known limitations

- **10/12-bit and HDR need `highp`** in fragment shaders. Without it the renderer refuses
  high-bit frames with a typed error instead of drawing them wrong.
- **10-bit DMA-BUF (P010) is not imported**: zero-copy import is 8-bit NV12 for hardware frames
  and 8/10/12-bit planar layouts for kernel buffers.
- **HDR is tone-mapped to SDR.** The 65³ table differs from the exact formula by at most 3/255.
  Color adjustment does not apply on the HDR path (as in Fastiplayer's Vulkan renderer).
- Everything listed for [`gles2-egui`](../gles2-egui/README.md#known-limitations) applies here too,
  including the shared `EGLDisplay` caveat.

Verified hardware and measurements: [GLES2 compatibility notes](../../docs/render/gles2-compatibility.md).

## Testing

Every path has a pixel test that renders offscreen and reads pixels back: colors per matrix and
range, geometry and letterbox, 10/12-bit, HDR, overlays, the upload ring and DMA-BUF imports
through real kernel buffers. Without EGL they are skipped with a printed reason;
`GLES2_EGUI_REQUIRE_EGL=1` turns that into a failure (CI runs them on Mesa llvmpipe).

## License

MIT, like the rest of Fastiplayer's first-party code (see [LICENSE](../../LICENSE)).
