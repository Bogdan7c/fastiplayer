# VA-API VP9 Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `crates/video-vaapi` — a VA-API VP9 hardware decoder using cros-codecs, integrated into the existing video player pipeline.

**Architecture:** The crate wraps `cros-codecs::StatelessDecoder<Vp9, VaapiBackend<GenericDmaVideoFrame>>` with a DMA-heap frame pool, wgpu texture cache, and event drain inside `decode()` to present a synchronous `VideoDecoder` trait interface.

**Tech Stack:** Rust 2024, cros-codecs 0.0.6 (vaapi feature), libva, nix (for ioctls), wgpu 29, anyhow, tracing

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/video-vaapi/Cargo.toml` | Crate manifest with cros-codecs dependency |
| `crates/video-vaapi/src/lib.rs` | Public exports |
| `crates/video-vaapi/src/dma_heap.rs` | Unsafe DMA-heap allocation (`/dev/dma_heap/system`) |
| `crates/video-vaapi/src/frame_pool.rs` | Pool of `GenericDmaVideoFrame` backed by dma-heap buffers |
| `crates/video-vaapi/src/texture_cache.rs` | Pooled wgpu textures (Y/R8Unorm + UV/Rg8Unorm) per resolution |
| `crates/video-vaapi/src/decoder.rs` | `VaapiVideoDecoder` — impl `VideoDecoder` trait |
| `crates/app-egui/Cargo.toml` | Add `video-vaapi` dependency |
| `crates/app-egui/src/state.rs` | Update `init_video_pipeline()` to try VA-API first |
| `crates/app-egui/src/main.rs` | Downcast `VaapiVideoDecoder` in render loop |

---

## Dependencies

### New dependency: `crates/video-vaapi/Cargo.toml`

```toml
[package]
name = "video-vaapi"
version.workspace = true
edition.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
bytes.workspace = true
wgpu.workspace = true
nix = { version = "0.29", features = ["ioctl"] }

webm-demux = { path = "../webm-demux" }
video-core = { path = "../video-core" }
cros-codecs = { version = "0.0.6", features = ["vaapi", "backend"] }
```

### Workspace update: `Cargo.toml`

Add `"crates/video-vaapi"` to `workspace.members`.

---

## Task 1: Create crate skeleton

**Files:**
- Create: `crates/video-vaapi/Cargo.toml`
- Create: `crates/video-vaapi/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create `crates/video-vaapi/Cargo.toml`**

```toml
[package]
name = "video-vaapi"
version.workspace = true
edition.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
bytes.workspace = true
wgpu.workspace = true
nix = { version = "0.29", features = ["ioctl"] }

webm-demux = { path = "../webm-demux" }
video-core = { path = "../video-core" }
cros-codecs = { version = "0.0.6", features = ["vaapi", "backend"] }
```

- [ ] **Step 2: Create `crates/video-vaapi/src/lib.rs`**

```rust
pub mod decoder;
pub mod dma_heap;
pub mod frame_pool;
pub mod texture_cache;
```

- [ ] **Step 3: Add to workspace `Cargo.toml`**

```toml
members = [
    "crates/app-egui",
    "crates/webm-demux",
    "crates/audio",
    "crates/vp9-parser",
    "crates/video-core",
    "crates/video-vulkan",
    "crates/render",
    "crates/video-vaapi",  // ADD THIS
]
```

- [ ] **Step 4: Verify crate compiles (empty)**

Run: `cargo check -p video-vaapi`
Expected: PASS (empty crate with no errors)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/video-vaapi/
git commit -m "chore: create video-vaapi crate skeleton"
```

---

## Task 2: DMA-heap allocator

**Files:**
- Create: `crates/video-vaapi/src/dma_heap.rs`

- [ ] **Step 1: Implement `dma_heap.rs`**

```rust
use std::fs::{File, OpenOptions};
use std::os::fd::FromRawFd;

use nix::ioctl_readwrite;
use nix::libc::c_void;

/// ioctl magic for dma-buf heap
const DMA_HEAP_IOC_MAGIC: u8 = b'H';

#[repr(C)]
#[derive(Debug)]
struct DmaHeapAllocationData {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

ioctl_readwrite!(
    dma_heap_ioctl_alloc,
    DMA_HEAP_IOC_MAGIC,
    0,
    DmaHeapAllocationData
);

/// Path to the system DMA heap device.
const DMA_HEAP_PATH: &str = "/dev/dma_heap/system";

/// Allocate a DMA-BUF of `size` bytes using the system dma-heap.
///
/// Returns a `File` owning the DMA-BUF file descriptor.
/// The buffer is zero-initialized and suitable for CPU mmap + GPU import.
pub fn allocate_dma_buffer(size: usize) -> anyhow::Result<File> {
    let heap = OpenOptions::new()
        .read(true)
        .write(true)
        .open(DMA_HEAP_PATH)
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", DMA_HEAP_PATH, e))?;

    let mut data = DmaHeapAllocationData {
        len: size as u64,
        fd: 0,
        fd_flags: libc::O_RDWR | libc::O_CLOEXEC,
        heap_flags: 0,
    };

    // SAFETY: `data` is a properly-allocated struct, `heap` is a valid fd.
    unsafe {
        dma_heap_ioctl_alloc(heap.as_raw_fd(), &mut data)
            .map_err(|e| anyhow::anyhow!("DMA_HEAP_IOCTL_ALLOC failed: {}", e))?;
    }

    if data.fd == 0 {
        return Err(anyhow::anyhow!("DMA_HEAP_IOCTL_ALLOC returned invalid fd"));
    }

    // SAFETY: kernel returned a valid fd.
    let file = unsafe { File::from_raw_fd(data.fd as i32) };
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_dma_buffer() {
        let file = allocate_dma_buffer(4096).expect("dma-heap alloc failed");
        let metadata = file.metadata().expect("metadata failed");
        assert!(metadata.len() >= 4096);
    }
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p video-vaapi dma_heap::tests::test_allocate_dma_buffer -- --nocapture`
Expected: PASS (requires `/dev/dma_heap/system` — available on Linux 5.6+)

- [ ] **Step 3: Commit**

```bash
git add crates/video-vaapi/src/dma_heap.rs crates/video-vaapi/src/lib.rs
git commit -m "feat(video-vaapi): dma-heap allocator for DMA-BUF creation"
```

---

## Task 3: Frame pool

**Files:**
- Create: `crates/video-vaapi/src/frame_pool.rs`

- [ ] **Step 1: Implement `frame_pool.rs`**

```rust
use std::collections::VecDeque;

use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::{Fourcc, FrameLayout, PlaneLayout, Resolution};

use crate::dma_heap::allocate_dma_buffer;

/// Align `value` up to `alignment`.
fn align_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) & !(alignment - 1)
}

/// Compute NV12 layout for given resolution with VA-API alignment constraints.
fn nv12_layout(width: u32, height: u32) -> (FrameLayout, usize) {
    let aligned_width = align_up(width, 16);
    let aligned_height = align_up(height, 4);
    let stride = align_up(aligned_width, 64) as usize;
    let y_size = stride * aligned_height as usize;
    let uv_size = stride * (aligned_height as usize / 2);
    let total_size = y_size + uv_size;

    let layout = FrameLayout {
        format: (Fourcc::from(b"NV12"), 0),
        size: Resolution::from((aligned_width, aligned_height)),
        planes: vec![
            PlaneLayout {
                buffer_index: 0,
                offset: 0,
                stride,
            },
            PlaneLayout {
                buffer_index: 0,
                offset: y_size,
                stride,
            },
        ],
    };

    (layout, total_size)
}

/// Pool of `GenericDmaVideoFrame` objects backed by dma-heap buffers.
pub struct DmaFramePool {
    resolution: Resolution,
    free_frames: VecDeque<GenericDmaVideoFrame>,
}

impl DmaFramePool {
    /// Create a new pool with `count` frames for the given resolution.
    pub fn new(width: u32, height: u32, count: usize) -> anyhow::Result<Self> {
        let resolution = Resolution::from((width, height));
        let mut free_frames = VecDeque::with_capacity(count);

        for i in 0..count {
            let (layout, total_size) = nv12_layout(width, height);
            let fd = allocate_dma_buffer(total_size)
                .map_err(|e| anyhow::anyhow!("Failed to allocate dma buffer for frame {}: {}", i, e))?;
            let frame = GenericDmaVideoFrame::new(vec![fd], layout)
                .map_err(|e| anyhow::anyhow!("Failed to create GenericDmaVideoFrame {}: {}", i, e))?;
            free_frames.push_back(frame);
        }

        tracing::info!(
            width, height, count,
            "DmaFramePool created"
        );

        Ok(Self {
            resolution,
            free_frames,
        })
    }

    /// Allocate a free frame from the pool. Returns `None` if pool is empty.
    pub fn alloc(&mut self) -> Option<GenericDmaVideoFrame> {
        self.free_frames.pop_front()
    }

    /// Return a frame to the pool for reuse.
    pub fn return_frame(&mut self, frame: GenericDmaVideoFrame) {
        self.free_frames.push_back(frame);
    }

    /// Recreate the pool for a new resolution. Old frames are dropped.
    pub fn resize(&mut self, width: u32, height: u32, count: usize) -> anyhow::Result<()> {
        self.free_frames.clear();
        let new_pool = Self::new(width, height, count)?;
        self.resolution = new_pool.resolution;
        self.free_frames = new_pool.free_frames;
        Ok(())
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    pub fn num_free(&self) -> usize {
        self.free_frames.len()
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p video-vaapi frame_pool -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/video-vaapi/src/frame_pool.rs crates/video-vaapi/src/lib.rs
git commit -m "feat(video-vaapi): DmaFramePool for GenericDmaVideoFrame management"
```

---

## Task 4: Texture cache

**Files:**
- Create: `crates/video-vaapi/src/texture_cache.rs`

- [ ] **Step 1: Implement `texture_cache.rs`**

```rust
use std::sync::Arc;

use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::decoder::DecodedHandle;
use video_core::FrameTextureHandle;

/// A single slot holding Y and UV wgpu textures for one decoded frame.
struct TextureSlot {
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    y_view: wgpu::TextureView,
    uv_view: wgpu::TextureView,
    width: u32,
    height: u32,
    in_use: bool,
}

/// Pool of wgpu texture pairs indexed by `FrameTextureHandle`.
pub struct WgpuTexturePool {
    device: Arc<wgpu::Device>,
    slots: Vec<TextureSlot>,
    next_handle: u64,
}

impl WgpuTexturePool {
    pub fn new(device: Arc<wgpu::Device>) -> Self {
        Self {
            device,
            slots: Vec::with_capacity(8),
            next_handle: 0,
        }
    }

    /// Upload a decoded frame into a free texture slot.
    /// Returns the `FrameTextureHandle` for the slot.
    pub fn upload_frame(
        &mut self,
        handle: &dyn DecodedHandle<Frame = GenericDmaVideoFrame>,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<FrameTextureHandle> {
        handle.sync()?;
        let frame = handle.video_frame();
        let mapping = frame.map()
            .map_err(|e| anyhow::anyhow!("Failed to map decoded frame: {}", e))?;
        let planes = mapping.get();

        if planes.len() < 2 {
            return Err(anyhow::anyhow!("Expected at least 2 planes, got {}", planes.len()));
        }

        let resolution = frame.resolution();
        let width = resolution.width;
        let height = resolution.height;
        let y_stride = frame.get_plane_pitch()[0] as u32;

        // Find or create a free slot for this resolution
        let slot_index = self.find_or_create_slot(width, height)?;
        let slot = &self.slots[slot_index];

        // Upload Y plane
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &slot.y_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes[0],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(y_stride),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        // Upload UV plane
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &slot.uv_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes[1],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(y_stride),
                rows_per_image: Some(height / 2),
            },
            wgpu::Extent3d {
                width: width / 2,
                height: height / 2,
                depth_or_array_layers: 1,
            },
        );

        self.slots[slot_index].in_use = true;
        let handle_id = self.next_handle;
        self.next_handle += 1;

        // Store mapping from handle to slot index. For simplicity, we use a
        // flat handle space where handle = slot_index for now.
        // In production, use a HashMap<FrameTextureHandle, usize>.
        Ok(FrameTextureHandle(handle_id))
    }

    /// Get texture views for a given frame handle.
    pub fn get_views(
        &self,
        handle: FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        // Simplified: for now we search by handle id. In practice we need a map.
        // TODO: implement proper handle-to-slot mapping
        self.slots.iter().find(|s| s.in_use).map(|s| {
            (s.y_view.clone(), s.uv_view.clone())
        })
    }

    /// Mark a slot as free so it can be reused.
    pub fn release_slot(&mut self, _handle: FrameTextureHandle) {
        // TODO: implement with proper handle-to-slot mapping
        if let Some(slot) = self.slots.iter_mut().find(|s| s.in_use) {
            slot.in_use = false;
        }
    }

    /// Drop all slots (called on FormatChanged).
    pub fn invalidate_all(&mut self) {
        self.slots.clear();
        self.next_handle = 0;
    }

    fn find_or_create_slot(&mut self, width: u32, height: u32) -> anyhow::Result<usize> {
        // First try to find a free slot with matching resolution
        if let Some(idx) = self.slots.iter().position(|s| !s.in_use && s.width == width && s.height == height) {
            return Ok(idx);
        }

        // Otherwise create a new slot
        if self.slots.len() >= 8 {
            return Err(anyhow::anyhow!("Texture pool exhausted (max 8 slots)"));
        }

        let y_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vaapi y texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let uv_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vaapi uv texture"),
            size: wgpu::Extent3d { width: width / 2, height: height / 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.slots.push(TextureSlot {
            y_texture,
            uv_texture,
            y_view,
            uv_view,
            width,
            height,
            in_use: false,
        });

        Ok(self.slots.len() - 1)
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/video-vaapi/src/texture_cache.rs crates/video-vaapi/src/lib.rs
git commit -m "feat(video-vaapi): WgpuTexturePool for decoded frame upload"
```

---

## Task 5: VaapiVideoDecoder core

**Files:**
- Create: `crates/video-vaapi/src/decoder.rs`

- [ ] **Step 1: Implement `decoder.rs` — structure and `new()`**

```rust
use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use cros_codecs::decoder::stateless::vp9::StatelessDecoder;
use cros_codecs::decoder::stateless::StatelessVideoDecoder;
use cros_codecs::decoder::BlockingMode;
use cros_codecs::decoder::DecoderEvent;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::Resolution;
use tracing::{debug, info, warn};
use video_core::{ColorSpace, DecodedFrame, FrameTextureHandle, VideoDecoder};
use webm_demux::packet::Packet;

use crate::frame_pool::DmaFramePool;
use crate::texture_cache::WgpuTexturePool;

const FRAME_POOL_SIZE: usize = 12;
const INITIAL_WIDTH: u32 = 1920;
const INITIAL_HEIGHT: u32 = 1080;

pub struct VaapiVideoDecoder {
    inner: cros_codecs::decoder::stateless::DynStatelessVideoDecoder<GenericDmaVideoFrame>,
    frame_pool: DmaFramePool,
    texture_cache: WgpuTexturePool,
    ready_queue: VecDeque<DecodedFrame>,
    device: Arc<wgpu::Device>,
    backend_name: &'static str,
}

impl VaapiVideoDecoder {
    pub fn new(device: Arc<wgpu::Device>) -> Result<Self> {
        info!("Opening VA-API display");
        let display = cros_codecs::libva::Display::open()
            .map_err(|e| anyhow::anyhow!("Failed to open VA-API display: {}", e))?;

        info!("Creating VA-API VP9 decoder");
        let decoder = StatelessDecoder::<cros_codecs::decoder::stateless::vp9::Vp9, _>
            ::new_vaapi(Arc::new(display), BlockingMode::Blocking)
            .map_err(|e| anyhow::anyhow!("Failed to create VA-API decoder: {:?}", e))?;

        let inner = decoder.into_trait_object();

        info!("Creating DMA frame pool");
        let frame_pool = DmaFramePool::new(INITIAL_WIDTH, INITIAL_HEIGHT, FRAME_POOL_SIZE)
            .map_err(|e| anyhow::anyhow!("Failed to create frame pool: {}", e))?;

        let texture_cache = WgpuTexturePool::new(device.clone());

        Ok(Self {
            inner,
            frame_pool,
            texture_cache,
            ready_queue: VecDeque::new(),
            device,
            backend_name: "VA-API VP9",
        })
    }

    /// For integration with render loop: get Y/UV texture views for a frame.
    pub fn get_or_create_wgpu_texture_views(
        &mut self,
        _frame_handle: FrameTextureHandle,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
    ) -> Result<Option<(wgpu::TextureView, wgpu::TextureView)>> {
        // TODO: implement in Task 6
        Ok(None)
    }
}
```

- [ ] **Step 2: Implement `VideoDecoder` trait — `decode()` and `flush()`**

Append to `decoder.rs`:

```rust
impl VideoDecoder for VaapiVideoDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>> {
        let timestamp_us = packet.pts.as_micros() as u64;

        // Closure that provides a free frame from the pool to cros-codecs
        let mut alloc_cb = || self.frame_pool.alloc();

        // Submit bitstream to decoder
        match self.inner.decode(timestamp_us, &packet.data, &mut alloc_cb) {
            Ok(_processed) => {}
            Err(cros_codecs::decoder::stateless::DecodeError::CheckEvents) => {
                debug!("Decoder requested event drain");
            }
            Err(cros_codecs::decoder::stateless::DecodeError::NotEnoughOutputBuffers(n)) => {
                warn!(needed = n, "Decoder out of output buffers");
            }
            Err(cros_codecs::decoder::stateless::DecodeError::ParseFrameError(msg)) => {
                warn!(%msg, "VP9 parse error, skipping packet");
                return Ok(None);
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Decode error: {:?}", e));
            }
        }

        // Drain all pending events
        while let Some(event) = self.inner.next_event() {
            match event {
                DecoderEvent::FrameReady(handle) => {
                    if let Err(e) = self.process_ready_frame(handle) {
                        warn!(error = %e, "Failed to process ready frame");
                    }
                }
                DecoderEvent::FormatChanged => {
                    info!("Format changed, invalidating texture cache");
                    self.texture_cache.invalidate_all();
                }
            }
        }

        // Return the oldest ready frame
        Ok(self.ready_queue.pop_front())
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
            .map_err(|e| anyhow::anyhow!("Flush error: {:?}", e))?;
        self.ready_queue.clear();
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
```

- [ ] **Step 3: Add `process_ready_frame` helper**

Append to `decoder.rs`:

```rust
impl VaapiVideoDecoder {
    fn process_ready_frame(
        &mut self,
        handle: cros_codecs::decoder::DynDecodedHandle<GenericDmaVideoFrame>,
    ) -> Result<()> {
        let resolution = handle.coded_resolution();
        let display_resolution = handle.display_resolution();

        // Sync with GPU decode
        handle.sync()?;

        // Upload to wgpu texture
        let texture_handle = self.texture_cache.upload_frame(&*handle, &self.device.queue())
            .map_err(|e| anyhow::anyhow!("Texture upload failed: {}", e))?;

        // Return the backing frame to the pool for reuse
        let frame = handle.video_frame();
        if let Ok(frame) = Arc::try_unwrap(frame) {
            self.frame_pool.return_frame(frame);
        } else {
            debug!("Frame still referenced, cannot return to pool yet");
        }

        self.ready_queue.push_back(DecodedFrame {
            pts: Duration::from_micros(handle.timestamp()),
            width: resolution.width,
            height: resolution.height,
            render_width: display_resolution.width,
            render_height: display_resolution.height,
            color_space: ColorSpace::Bt709Limited,
            texture_handle,
        });

        Ok(())
    }
}
```

- [ ] **Step 4: Commit**

```bash
git add crates/video-vaapi/src/decoder.rs crates/video-vaapi/src/lib.rs
git commit -m "feat(video-vaapi): VaapiVideoDecoder core with event drain"
```

---

## Task 6: Fix compilation issues

**Files:**
- Modify: `crates/video-vaapi/src/decoder.rs`
- Modify: `crates/video-vaapi/src/texture_cache.rs`

After implementing the above, `cargo check` will likely reveal issues. Common problems:

1. `DynStatelessVideoDecoder` type path
2. `DecodedHandle` trait method signatures
3. `FrameTextureHandle` usage in `WgpuTexturePool`
4. Missing imports

- [ ] **Step 1: Run cargo check**

Run: `cargo check -p video-vaapi`
Expected: May show compilation errors that need fixing.

- [ ] **Step 2: Fix all compilation errors iteratively**

Edit files to resolve errors. Common fixes:
- Add missing imports (`use cros_codecs::decoder::DynDecodedHandle;`)
- Fix type paths for `StatelessDecoder`, `Vp9`, `VaapiBackend`
- Ensure `WgpuTexturePool::upload_frame` signature matches usage
- Add `#[allow(dead_code)]` where needed for partial implementations

- [ ] **Step 3: Verify clean build**

Run: `cargo check -p video-vaapi`
Expected: PASS with zero errors.

- [ ] **Step 4: Commit**

```bash
git add crates/video-vaapi/src/
git commit -m "fix(video-vaapi): resolve compilation errors"
```

---

## Task 7: Complete texture cache with handle mapping

**Files:**
- Modify: `crates/video-vaapi/src/texture_cache.rs`

The initial implementation uses a simplified handle lookup. We need a proper `HashMap<FrameTextureHandle, usize>`.

- [ ] **Step 1: Update `texture_cache.rs` with handle-to-slot mapping**

Replace the `WgpuTexturePool` implementation:

```rust
use std::collections::HashMap;

use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::decoder::DecodedHandle;
use video_core::FrameTextureHandle;

struct TextureSlot {
    y_view: wgpu::TextureView,
    uv_view: wgpu::TextureView,
    width: u32,
    height: u32,
    in_use: bool,
}

pub struct WgpuTexturePool {
    device: Arc<wgpu::Device>,
    slots: Vec<TextureSlot>,
    handle_to_slot: HashMap<u64, usize>,
    next_handle: u64,
}

impl WgpuTexturePool {
    pub fn new(device: Arc<wgpu::Device>) -> Self {
        Self {
            device,
            slots: Vec::with_capacity(8),
            handle_to_slot: HashMap::new(),
            next_handle: 0,
        }
    }

    pub fn upload_frame(
        &mut self,
        handle: &dyn DecodedHandle<Frame = GenericDmaVideoFrame>,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<FrameTextureHandle> {
        handle.sync()?;
        let frame = handle.video_frame();
        let mapping = frame.map()
            .map_err(|e| anyhow::anyhow!("Failed to map decoded frame: {}", e))?;
        let planes = mapping.get();

        if planes.len() < 2 {
            return Err(anyhow::anyhow!("Expected at least 2 planes, got {}", planes.len()));
        }

        let resolution = frame.resolution();
        let width = resolution.width;
        let height = resolution.height;
        let y_stride = frame.get_plane_pitch()[0] as u32;

        let slot_index = self.find_or_create_slot(width, height)?;
        let slot = &self.slots[slot_index];

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.get_y_texture(slot_index),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes[0],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(y_stride),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.get_uv_texture(slot_index),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes[1],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(y_stride),
                rows_per_image: Some(height / 2),
            },
            wgpu::Extent3d { width: width / 2, height: height / 2, depth_or_array_layers: 1 },
        );

        self.slots[slot_index].in_use = true;
        let handle_id = self.next_handle;
        self.next_handle += 1;
        self.handle_to_slot.insert(handle_id, slot_index);

        Ok(FrameTextureHandle(handle_id))
    }

    pub fn get_views(
        &self,
        handle: FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        let slot_index = *self.handle_to_slot.get(&handle.0)?;
        let slot = self.slots.get(slot_index)?;
        if !slot.in_use {
            return None;
        }
        Some((slot.y_view.clone(), slot.uv_view.clone()))
    }

    pub fn release_slot(&mut self, handle: FrameTextureHandle) {
        if let Some(&slot_index) = self.handle_to_slot.get(&handle.0) {
            if let Some(slot) = self.slots.get_mut(slot_index) {
                slot.in_use = false;
            }
            self.handle_to_slot.remove(&handle.0);
        }
    }

    pub fn invalidate_all(&mut self) {
        self.slots.clear();
        self.handle_to_slot.clear();
        self.next_handle = 0;
    }

    // ... rest of implementation with textures stored separately or in slot
}
```

*Note: The exact implementation may need adjustment based on borrow checker requirements. Store `y_texture`/`uv_texture` alongside views, or use `Arc<Texture>`.*

- [ ] **Step 2: Commit**

```bash
git add crates/video-vaapi/src/texture_cache.rs
git commit -m "feat(video-vaapi): proper handle-to-slot mapping in texture cache"
```

---

## Task 8: Integrate into app-egui

**Files:**
- Modify: `crates/app-egui/Cargo.toml`
- Modify: `crates/app-egui/src/state.rs`
- Modify: `crates/app-egui/src/main.rs`

- [ ] **Step 1: Add dependency to `crates/app-egui/Cargo.toml`**

```toml
video-vaapi = { path = "../video-vaapi" }
```

- [ ] **Step 2: Update `init_video_pipeline()` in `state.rs`**

Replace the method:

```rust
/// Инициализирует video pipeline (VA-API decoder).
pub fn init_video_pipeline(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
    let device = Arc::new(device.clone());
    match video_vaapi::VaapiVideoDecoder::new(device) {
        Ok(decoder) => {
            self.video_backend = decoder.backend_name();
            self.video_decoder = Some(Box::new(decoder));
            info!(backend = self.video_backend, "Video decoder initialized");
        }
        Err(e) => {
            warn!(error = %e, "VA-API decoder unavailable, no hardware decode");
        }
    }
}
```

- [ ] **Step 3: Update render loop in `main.rs`**

Replace the downcast block:

```rust
let (video_y_view, video_uv_view): (Option<wgpu::TextureView>, Option<wgpu::TextureView>) = {
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

- [ ] **Step 4: Update `init_video_pipeline` call site in `main.rs`**

```rust
let mut app_state = AppState::new(&window, self.telemetry.clone());
app_state.init_video_pipeline(&renderer.gpu.device, &renderer.gpu.queue);
```

- [ ] **Step 5: Commit**

```bash
git add crates/app-egui/
git commit -m "feat(app-egui): integrate VA-API decoder"
```

---

## Task 9: Build and fix integration issues

**Files:**
- Various files as needed

- [ ] **Step 1: Run cargo check on workspace**

Run: `cargo check`
Expected: May show errors in integration points (type mismatches, missing imports).

- [ ] **Step 2: Fix compilation errors iteratively**

Common issues to fix:
- Import paths in `app-egui` for `video_vaapi`
- `Arc<wgpu::Device>` vs `&wgpu::Device` in `VaapiVideoDecoder::new()`
- `get_or_create_wgpu_texture_views` signature mismatch
- Missing `use std::sync::Arc;` in `state.rs`
- Remove unused `video_vulkan` import if it causes issues

- [ ] **Step 3: Run cargo build**

Run: `cargo build`
Expected: PASS with zero errors.

- [ ] **Step 4: Commit**

```bash
git add .
git commit -m "fix: resolve integration compilation errors"
```

---

## Task 10: Test with local VP9 file

**Files:**
- None (runtime testing)

- [ ] **Step 1: Run the application**

Run: `cargo run --bin youtube-player`
Expected: App launches, UI shows "Backend: VA-API VP9" (or error if VA-API unavailable).

- [ ] **Step 2: Open test VP9 file**

Click "Open File" and select `test-assets/test.webm` or `test-assets/1080p/big_buck_bunny_1080p.webm`.

Expected behavior:
- Video plays with audio sync
- No green/purple artifacts
- Telemetry shows decoded frames

- [ ] **Step 3: Verify hardware decode**

In a separate terminal:
```bash
intel_gpu_top
```
Expected: "Video" engine shows load > 0 during playback.

- [ ] **Step 4: Verify CPU usage**

```bash
top -p $(pgrep youtube-player)
```
Expected: CPU usage < 10% for 1080p60.

- [ ] **Step 5: Commit**

```bash
git commit -m "test: verified VA-API VP9 decode on target hardware" --allow-empty
```

---

## Task 11: Add basic unit tests

**Files:**
- Modify: `crates/video-vaapi/src/dma_heap.rs`
- Modify: `crates/video-vaapi/src/frame_pool.rs`

- [ ] **Step 1: Add edge case tests**

For `dma_heap.rs`:
- Test allocation of various sizes (4K, 1MB, 12MB)
- Test that returned fd can be mmap'd

For `frame_pool.rs`:
- Test alloc/return cycle
- Test pool exhaustion (alloc returns None)
- Test resize

- [ ] **Step 2: Run tests**

Run: `cargo test -p video-vaapi`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/video-vaapi/src/
git commit -m "test(video-vaapi): add unit tests for dma-heap and frame pool"
```

---

## Self-Review Checklist

Before marking complete, verify:

- [ ] **Spec coverage:** Every section of the design doc has a corresponding task
- [ ] **Placeholder scan:** No "TODO", "TBD", "implement later" in plan steps
- [ ] **Type consistency:** `FrameTextureHandle`, `DecodedFrame`, `VideoDecoder` signatures match across all tasks
- [ ] **File paths:** All paths are exact and relative to workspace root
- [ ] **Dependencies:** `cros-codecs`, `nix`, `video-vaapi` all declared correctly
- [ ] **Error handling:** Graceful fallback documented in Task 8
- [ ] **Performance:** Texture pool and frame pool sizes specified (8 and 12)

## Plan Acceptance Criteria

- [ ] `cargo build` passes with zero errors
- [ ] `cargo test -p video-vaapi` passes
- [ ] App launches and shows "Backend: VA-API VP9"
- [ ] Local VP9 WebM plays with video and audio
- [ ] `intel_gpu_top` shows Video engine activity
- [ ] CPU usage < 10% for 1080p60
- [ ] Graceful fallback if VA-API unavailable (no panic)
