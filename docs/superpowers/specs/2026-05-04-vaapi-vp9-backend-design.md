# VA-API VP9 Backend Design

**Date:** 2026-05-04
**Status:** Approved
**Scope:** `crates/video-vaapi` — hardware VP9 decode via VA-API using cros-codecs

## Goals

- Provide a `VideoDecoder` implementation for the existing `video-core` trait
- Use VA-API via `cros-codecs` for hardware VP9 decode on Intel UHD 620
- Export decoded NV12 frames through DMA-BUF CPU map → wgpu texture upload
- Maintain < 10% CPU for 1080p60 VP9 playback
- Graceful fallback if VA-API is unavailable

## Non-Goals

- Support for Vulkan Video (deprecated on this hardware)
- Zero-copy DMA-BUF → wgpu interop (not feasible without Vulkan external memory)
- Software VP9 decode fallback
- HDR/10-bit color (initially SDR only)

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  App (app-egui)                                             │
│  - process_pending_video_packets()                          │
│  - render_frame() downcast + get_or_create_wgpu_texture()   │
└─────────────────────┬───────────────────────────────────────┘
                      │ VideoDecoder trait
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  video-vaapi                                                │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ VaapiVideoDecoder (impl VideoDecoder)                │   │
│  │ - decode(): submit + drain events + return frame     │   │
│  │ - flush(): decoder.flush()                           │   │
│  │ - get_or_create_wgpu_texture(): Y/UV views           │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ cros-codecs StatelessDecoder<Vp9, VaapiBackend>      │   │
│  │ - decode(timestamp, bitstream, alloc_cb)             │   │
│  │ - next_event(): FrameReady / FormatChanged           │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ DmaFramePool                                           │   │
│  │ - 12 GenericDmaVideoFrame backed by dma-heap         │   │
│  │ - alloc_cb returns free frame from pool              │   │
│  │ - return_frame() puts frame back                     │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ WgpuTexturePool                                        │   │
│  │ - 8 texture slots per resolution                     │   │
│  │ - write_texture() for Y + UV planes                  │   │
│  │ - invalidate on FormatChanged                        │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ DmaHeapAllocator (unsafe module)                     │   │
│  │ - open /dev/dma_heap/system                          │   │
│  │ - DMA_HEAP_IOCTL_ALLOC                               │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## Key Decisions

### 1. Frame Pool: dma-heap backed GenericDmaVideoFrame

**Decision:** Create `GenericDmaVideoFrame` pool manually using `/dev/dma_heap/system`.

Rationale:
- `StatelessDecoder` requires caller-managed output buffers via `alloc_cb`
- `GenericDmaVideoFrame` is the only `VideoFrame` type in cros-codecs that supports VA-API DMA-BUF export
- dma-heap is standard Linux API since 5.6, no extra dependencies
- Intel i965 driver supports `VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`

Frame layout: NV12 with 2 planes
- Plane 0 (Y): offset=0, stride=align_up(width, 64)
- Plane 1 (UV): offset=stride * align_up(height, 4), stride=same as Y

Pool size: 12 frames (matches `NUM_SURFACES` in cros-codecs VP9 decoder)

### 2. Event Loop Integration: drain in decode()

**Decision:** `VaapiVideoDecoder::decode()` submits bitstream, immediately drains all events, processes ready frames, and returns the oldest decoded frame.

```rust
fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>> {
    // Submit bitstream to cros-codecs
    match self.inner.decode(timestamp, data, &mut alloc_cb) {
        Ok(_) | Err(DecodeError::CheckEvents) => {}
        Err(e) => return Err(e.into()),
    }
    
    // Drain all events
    while let Some(event) = self.inner.next_event() {
        match event {
            DecoderEvent::FrameReady(handle) => {
                self.process_ready_frame(handle)?;
            }
            DecoderEvent::FormatChanged => {
                self.texture_cache.invalidate_all();
            }
        }
    }
    
    // Return oldest ready frame
    Ok(self.ready_queue.pop_front())
}
```

Rationale:
- Zero changes to existing `process_pending_video_packets()` in `app-egui`
- Simpler mental model: `decode()` is synchronous from caller's perspective
- `process_ready_frame` includes sync + map + upload — unavoidable latency

### 3. Texture Strategy: pooled per-resolution

**Decision:** Maintain a pool of 8 wgpu texture pairs per resolution. Texture pairs are indexed by `FrameTextureHandle`.

```rust
struct TextureSlot {
    y_texture: wgpu::Texture,      // R8Unorm
    uv_texture: wgpu::Texture,     // Rg8Unorm
    y_view: wgpu::TextureView,
    uv_view: wgpu::TextureView,
    resolution: Resolution,
    in_use: bool,
}

struct WgpuTexturePool {
    device: wgpu::Device, // stored for recreation
    slots: Vec<TextureSlot>,
}
```

Allocation strategy:
- First decode for a new resolution: allocate `TextureSlot`
- Subsequent decodes: reuse free slot or allocate new (up to max 8)
- When `DecodedFrame` is dropped, mark slot as free
- On `FormatChanged`: drop all slots for old resolution

Rationale:
- `write_texture()` requires existing texture — allocation on every frame causes stutter
- 8 slots × 1080p NV12 ≈ 24 MB — acceptable memory overhead
- Integrated GPU shares RAM anyway

### 4. Frame Export Path

```
DecoderEvent::FrameReady(handle)
    ↓
handle.sync() — ждём завершение GPU decode
    ↓
handle.video_frame() → Arc<GenericDmaVideoFrame>
    ↓
frame.map() → DmaMapping → Vec<&[u8]> (Y + UV planes)
    ↓
queue.write_texture() → wgpu Texture (Y plane, R8Unorm)
queue.write_texture() → wgpu Texture (UV plane, Rg8Unorm)
    ↓
DecodedFrame { texture_handle: slot_index, ... }
```

For Intel UHD 620 (integrated GPU):
- DMA-BUF is shared system memory — `mmap()` has near-zero cost
- `write_texture()` copies within the same physical RAM
- Total bandwidth: 1080p60 NV12 ≈ 186 MB/s — negligible vs 25 GB/s DDR4

## Data Flow

### Decode Flow

```
process_pending_video_packets()
    ↓
VaapiVideoDecoder::decode(packet)
    ├─→ vp9_parser::parse_uncompressed_header() [optional, for keyframe info]
    ├─→ inner.decode(timestamp, data, alloc_cb)
    │     ├─→ alloc_cb → DmaFramePool::alloc() → GenericDmaVideoFrame
    │     └─→ VA-API submits decode job
    ├─→ drain events
    │     ├─→ FrameReady → sync + map + upload → push to ready_queue
    │     └─→ FormatChanged → texture_cache.invalidate_all()
    └─→ return ready_queue.pop_front()
```

### Render Flow

```
render_frame()
    ↓
downcast VideoDecoder → VaapiVideoDecoder
    ↓
get_or_create_wgpu_texture_views(frame_handle)
    └─→ texture_pool.get_views(slot_index) → (y_view, uv_view)
    ↓
Nv12VideoRenderer::render_frame(y_view, uv_view, ...)
    ↓
present
```

## Error Handling

| Error | Handling |
|-------|----------|
| `libva::Display::open()` fails | Return `Err` from `VaapiVideoDecoder::new()`. App falls back to no video decoder. |
| `StatelessDecoder::new_vaapi()` fails | Same as above. |
| `decode()` returns `ParseFrameError` | Log warning, skip packet, continue. |
| `decode()` returns `BackendError` | Log error, skip packet. After N consecutive errors, return error to app. |
| `sync()` fails | Log error, drop frame, return frame to pool. |
| `map()` fails | Log error, drop frame, return frame to pool. |
| `FormatChanged` mid-stream | Invalidate texture cache, continue. Next keyframe resumes decode. |
| Frame pool exhausted | `alloc_cb` returns `None` → `decode()` returns `NotEnoughOutputBuffers` → caller should drain events. But we drain events in `decode()`, so this should not happen unless all 12 frames are held by the app simultaneously. |

## API Surface

### `video-vaapi/src/lib.rs`

```rust
pub use decoder::VaapiVideoDecoder;
```

### `video-vaapi/src/decoder.rs`

```rust
pub struct VaapiVideoDecoder {
    inner: DynStatelessVideoDecoder<GenericDmaVideoFrame>,
    frame_pool: DmaFramePool,
    texture_cache: WgpuTexturePool,
    ready_queue: VecDeque<DecodedFrame>,
    pending_return: Vec<GenericDmaVideoFrame>,
    backend_name: &'static str,
}

impl VaapiVideoDecoder {
    pub fn new() -> anyhow::Result<Self>;
    
    pub fn get_or_create_wgpu_texture_views(
        &mut self,
        frame_handle: FrameTextureHandle,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<Option<(wgpu::TextureView, wgpu::TextureView)>>;
}

impl VideoDecoder for VaapiVideoDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>>;
    fn flush(&mut self) -> Result<()>;
    fn backend_name(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
```

### `video-vaapi/src/frame_pool.rs`

```rust
pub struct DmaFramePool {
    free_frames: Vec<GenericDmaVideoFrame>,
    allocated_frames: usize,
}

impl DmaFramePool {
    pub fn new(resolution: Resolution, count: usize) -> anyhow::Result<Self>;
    pub fn alloc(&mut self) -> Option<GenericDmaVideoFrame>;
    pub fn return_frame(&mut self, frame: GenericDmaVideoFrame);
    pub fn resize(&mut self, resolution: Resolution) -> anyhow::Result<()>;
}
```

### `video-vaapi/src/texture_cache.rs`

```rust
pub struct WgpuTexturePool {
    device: wgpu::Device,
    slots: Vec<TextureSlot>,
    next_handle: u64,
}

impl WgpuTexturePool {
    pub fn new(device: wgpu::Device) -> Self;
    
    pub fn upload_frame(
        &mut self,
        handle: &dyn DecodedHandle<Frame = GenericDmaVideoFrame>,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<FrameTextureHandle>;
    
    pub fn get_views(
        &self,
        handle: FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)>;
    
    pub fn release_slot(&mut self, handle: FrameTextureHandle);
    pub fn invalidate_all(&mut self);
}
```

### `video-vaapi/src/dma_heap.rs`

```rust
pub fn allocate_dma_buffer(size: usize) -> anyhow::Result<std::fs::File>;
```

Unsafe module. Opens `/dev/dma_heap/system`, performs `DMA_HEAP_IOCTL_ALLOC`.

## Integration Points

### `crates/app-egui/Cargo.toml`

```toml
video-vaapi = { path = "../video-vaapi" }
```

### `crates/app-egui/src/state.rs`

Replace `init_video_pipeline()`:

```rust
pub fn init_video_pipeline(&mut self) {
    match video_vaapi::VaapiVideoDecoder::new() {
        Ok(decoder) => {
            self.video_backend = decoder.backend_name();
            self.video_decoder = Some(Box::new(decoder));
            info!(backend = self.video_backend, "Video decoder initialized");
        }
        Err(e) => {
            warn!(error = %e, "VA-API decoder unavailable");
        }
    }
}
```

### `crates/app-egui/src/main.rs`

Replace the downcast block in `render_frame()`:

```rust
let (video_y_view, video_uv_view) = {
    if let Some(ref mut decoder) = app_state.video_decoder {
        if let Some(vaapi_decoder) = decoder.as_any_mut().downcast_mut::<video_vaapi::VaapiVideoDecoder>() {
            if let Some(ref frame) = app_state.present_video_frame {
                match vaapi_decoder.get_or_create_wgpu_texture_views(
                    frame.texture_handle,
                    &renderer.gpu.device,
                    &renderer.gpu.queue,
                ) {
                    Ok(Some((y_view, uv_view))) => (Some(y_view), Some(uv_view)),
                    _ => (None, None),
                }
            } else { (None, None) }
        } else { (None, None) }
    } else { (None, None) }
};
```

## Performance Considerations

- **Integrated GPU optimization:** CPU map of DMA-BUF is near-zero-cost on UHD 620 (shared memory)
- **Texture reuse:** Pool eliminates per-frame wgpu texture allocation
- **Blocking mode:** `BlockingMode::Blocking` simplifies sync logic — decode waits for GPU completion
- **Batching:** `decode()` drains all events, so multiple frames can be processed per call
- **Memory:** ~36 MB total for frame pool (12 × 3 MB) + texture pool (8 × 3 MB) = ~72 MB worst case

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| dma-heap not available on some systems | Check `/dev/dma_heap/system` at startup. If missing, return error. |
| Intel i965 driver doesn't support DRM_PRIME_2 | Test early with `vainfo`. cros-codecs will fail gracefully at surface creation. |
| Frame pool starvation (all 12 frames in use) | App holds at most ~3 frames (queue + present). 12 is ample headroom. |
| DRC causes visible glitch | `FormatChanged` invalidates cache. Next keyframe resumes. Brief artifact acceptable for MVP. |
| `write_texture` bandwidth bottleneck | For 1080p60: 186 MB/s << 25 GB/s DDR4. Not a concern. |

## Testing Strategy

1. **Unit tests:**
   - `DmaHeapAllocator::allocate_dma_buffer()` — verify fd is valid, can be mmap'd
   - `DmaFramePool::alloc()` / `return_frame()` — pool exhaustion and refill
   - `WgpuTexturePool` — slot allocation, reuse, invalidate

2. **Integration tests:**
   - `VaapiVideoDecoder::new()` — verify succeeds on target hardware
   - Decode single VP9 packet — verify `DecodedFrame` returned
   - Decode stream from `test-assets/test.webm` — verify no panics

3. **Hardware verification:**
   - `intel_gpu_top` — verify Video engine load > 0 during playback
   - `top` — verify CPU < 10% for 1080p60
   - Visual: no green/purple frames, correct aspect ratio

## Open Questions (resolved)

1. ~~How to create GenericDmaVideoFrame?~~ → dma-heap allocation
2. ~~Texture caching?~~ → Pooled per-resolution, 8 slots
3. ~~Event loop integration?~~ → Drain events inside decode()
4. ~~FormatChanged handling?~~ → Invalidate texture cache, continue
5. ~~Graceful fallback?~~ → Return error from new(), app handles
