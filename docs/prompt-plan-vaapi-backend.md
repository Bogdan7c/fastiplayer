# Промпт: Планирование реализации VA-API VP9 backend (video-vaapi)

## Контекст проекта

Мы разрабатываем нативный YouTube-плеер на Rust. Проект находится в `<REPO_ROOT>/`.

**Целевое железо:** ThinkPad T480s, Intel UHD 620 (Kaby Lake), Linux/Mesa/ANV.  
**Ключевое ограничение:** `VK_KHR_video_decode_vp9` НЕ доступен на этом GPU. Вместо этого используем **VA-API**.

**Результаты проверки:**
```bash
$ vainfo | grep vp9
VAProfileVP9Profile0 : VAEntrypointVLD
VAProfileVP9Profile0 : VAEntrypointEncSlice
VAProfileVP9Profile2 : VAEntrypointVLD
```

**Архитектурный документ:** `<REPO_ROOT>/youtube-player-architecture-updated.md` (обновлён 2026-05-04, переход с Vulkan Video на VA-API).

## Текущее состояние кодовой базы

Работающие компоненты:
- `crates/app-egui/` — winit + wgpu + egui приложение, рендер-цикл, UI
- `crates/webm-demux/` — WebM/Matroska demux через symphonia
- `crates/audio/` — Opus decode + CPAL output + audio clock
- `crates/vp9-parser/` — VP9 uncompressed header parser
- `crates/video-core/` — `VideoDecoder` trait + `AvSync`
- `crates/render/` — `Nv12VideoRenderer` (WGSL shader NV12→RGB)
- `crates/video-vulkan/` — Vulkan Video backend (deprecated, не работает на UHD 620)

### VideoDecoder trait (video-core/src/decoder.rs)
```rust
pub trait VideoDecoder {
    fn decode(&mut self, packet: &Packet) -> anyhow::Result<Option<DecodedFrame>>;
    fn flush(&mut self) -> anyhow::Result<()>;
    fn backend_name(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub struct DecodedFrame {
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub color_space: ColorSpace,
    pub texture_handle: FrameTextureHandle,
}
```

### Packet (webm-demux/src/packet.rs)
```rust
pub struct Packet {
    pub track_id: u32,
    pub kind: TrackKind,
    pub pts: Duration,
    pub dts: Option<Duration>,
    pub keyframe: bool,
    pub data: bytes::Bytes,
}
```

### Как video pipeline работает сейчас (app-egui/src/main.rs)

```rust
// В render_frame():
1. Demuxer читает packets → app_state.pending_video_packets
2. process_pending_video_packets() вызывает decoder.decode(packet)
3. DecodedFrame попадает в video_frame_queue
4. AvSync::decide(frame.pts, audio_clock) → Present/Wait/Drop
5. Если Present → app_state.present_video_frame = Some(frame)
6. В render loop:
   - downcast VideoDecoder → VaapiVideoDecoder (будет)
   - получить texture views для decoded frame
   - renderer.render_frame(time, present_frame, y_view, uv_view, ...)
```

### Render integration (crates/app-egui/src/main.rs, строки 367-389)

Сейчас используется downcast для `VulkanVideoDecoder`:
```rust
if let Some(ref mut decoder) = app_state.video_decoder {
    if let Some(vulkan_decoder) = decoder.as_any_mut().downcast_mut::<video_vulkan::VulkanVideoDecoder>() {
        if let Some(ref frame) = app_state.present_video_frame {
            let slot_index = frame.texture_handle.0 as usize;
            match unsafe { vulkan_decoder.get_or_create_wgpu_texture(slot_index, &renderer.gpu.device) } {
                Ok(Some((y_view, uv_view))) => (Some(y_view), Some(uv_view)),
                _ => (None, None),
            }
        } else { (None, None) }
    } else { (None, None) }
} else { (None, None) }
```

Нужно будет аналогично downcast'ить на `video_vaapi::VaapiVideoDecoder`.

## Что нужно спланировать

### 1. Новый crate: `crates/video-vaapi/`

**Цель:** Реализовать `VaapiVideoDecoder` через `cros-codecs` + `libva`.

**Зависимости (Cargo.toml):**
```toml
[package]
name = "video-vaapi"
version.workspace = true
edition.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
bytes.workspace = true
webm-demux = { path = "../webm-demux" }
video-core = { path = "../video-core" }
cros-codecs = { version = "0.0.6", features = ["vaapi"] }
```

**API surface:**
```rust
pub struct VaapiVideoDecoder {
    // cros-codecs stateless decoder
    // frame pool (GenericDmaVideoFrame)
    // pending output frames queue
    // wgpu textures cache (resize on format change)
}

impl VaapiVideoDecoder {
    pub fn new() -> anyhow::Result<Self>;
    
    // Для интеграции с render loop:
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

### 2. Интеграция в app-egui

**Изменения:**
- `crates/app-egui/Cargo.toml` — добавить `video-vaapi = { path = "../video-vaapi" }`
- `crates/app-egui/src/state.rs` — в `init_video_pipeline()`:
  ```rust
  // Сначала пробуем VA-API
  match video_vaapi::VaapiVideoDecoder::new() {
      Ok(decoder) => {
          self.video_backend = decoder.backend_name();
          self.video_decoder = Some(Box::new(decoder));
      }
      Err(e) => {
          warn!(error = %e, "VA-API decoder failed, no hardware decode available");
          // Оставляем video_decoder = None
      }
  }
  ```
- `crates/app-egui/src/main.rs` — в render loop заменить/дополнить downcast с `VulkanVideoDecoder` на `VaapiVideoDecoder`

### 3. Ключевые технические вопросы для планирования

**A. Frame Pool Management**

`cros-codecs` `StatelessDecoder` требует callback для выделения output frames:
```rust
decoder.decode(timestamp, bitstream, &mut || {
    // Вернуть свободный frame из pool, или None если pool пуст
    self.frame_pool.alloc()
})?;
```

Как организовать пул `GenericDmaVideoFrame`? 
- Сколько фреймов (рекомендуется 12 для VP9)?
- Как создавать DMA-BUF backed frames? (через GBM? dma-heap? или cros-codecs сам управляет?)
- Как возвращать фреймы в пул после использования?

**B. Frame Export → wgpu Texture**

После `DecoderEvent::FrameReady(handle)`:
1. `handle.sync()` — ждём завершения decode
2. `handle.video_frame().map()` — получаем `DmaMapping` с `Vec<&[u8]>`
3. `planes[0]` = Y, `planes[1]` = UV (interleaved)
4. `queue.write_texture()` → upload в wgpu

Нужно ли кэшировать wgpu textures? Или создавать каждый кадр?
- Для 1080p60 создание texture на каждый кадр может быть дорого
- Лучше: кэшировать по размеру, инвалидировать при смене разрешения

**C. Event Loop Integration**

`StatelessDecoder` API — pull-based (events), не push-based:
```rust
loop {
    match decoder.next_event() {
        Some(DecoderEvent::FrameReady(handle)) => { /* process */ }
        Some(DecoderEvent::FormatChanged) => { /* recreate textures */ }
        None => break,
    }
}
```

Как интегрировать это в существующий decode loop в `process_pending_video_packets()`?
- Вариант 1: `decode()` вызывает `decoder.decode()` + сразу drain'ит все events
- Вариант 2: отдельный "event pump" в render loop

**D. Format Changed Events**

При смене разрешения в потоке (DRC — Dynamic Resolution Change):
- `DecoderEvent::FormatChanged`
- Нужно пересоздать frame pool и wgpu textures
- Как это синхронизировать с `AvSync` и `video_frame_queue`?

**E. Error Handling**

Graceful degradation:
- Если `libva::Display::open()` fails — вернуть ошибку
- Если `StatelessDecoder::new_vaapi()` fails — вернуть ошибку
- Если decode fails — log warning, skip packet (не panic!)

### 4. Контекст cros-codecs API

Изучённые API (из `<HOME>/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/cros-codecs-0.0.6/`):

**Создание decoder:**
```rust
use cros_codecs::decoder::stateless::vp9::StatelessDecoder;
use cros_codecs::decoder::stateless::StatelessVideoDecoder;
use cros_codecs::decoder::BlockingMode;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;

let display = libva::Display::open()?;
let decoder = StatelessDecoder::<Vp9, VaapiBackend<GenericDmaVideoFrame>>
    ::new_vaapi(display, BlockingMode::Blocking)?;
```

**Decode:**
```rust
decoder.decode(timestamp: u64, bitstream: &[u8], alloc_cb: &mut dyn FnMut() -> Option<Frame>) 
    -> Result<usize, DecodeError>;
```

**Events:**
```rust
decoder.next_event() -> Option<DecoderEvent<Handle>>;

enum DecoderEvent<H> {
    FrameReady(H),
    FormatChanged,
}
```

**DecodedHandle:**
```rust
trait DecodedHandle {
    type Frame: VideoFrame;
    fn video_frame(&self) -> Arc<Self::Frame>;
    fn timestamp(&self) -> u64;
    fn sync(&self) -> anyhow::Result<()>;
    fn is_ready(&self) -> bool;
}
```

**VideoFrame (GenericDmaVideoFrame):**
```rust
trait VideoFrame {
    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String>;
    fn resolution(&self) -> Resolution;
    fn fourcc(&self) -> Fourcc;
}

trait ReadMapping<'a> {
    fn get(&self) -> Vec<&[u8]>;
}
```

## Инструкции по планированию

### Обязательные шаги

1. **Прочитать архитектурный документ** — `<REPO_ROOT>/youtube-player-architecture-updated.md`
2. **Изучить существующий код** — особенно:
   - `crates/video-core/src/decoder.rs` — VideoDecoder trait
   - `crates/app-egui/src/state.rs` — init_video_pipeline()
   - `crates/app-egui/src/main.rs` — render loop, process_pending_video_packets()
   - `crates/render/src/nv12_renderer.rs` — как используются Y/UV views
3. **Проверить документацию cros-codecs** через Context7
4. **Создать детальный план реализации** с:
   - Этапами (какой файл/модуль за что отвечает)
   - Зависимостями между этапами
   - Acceptance criteria для каждого этапа
   - Рисками и fallback'ами

### Использование навыков

**Обязательно использовать навыки:**
- `brainstorming` — перед началом планирования для обсуждения архитектурных решений
- `writing-plans` — для создания структурированного плана
- `context7` — для проверки актуальной документации cros-codecs, libva, wgpu

### Вопросы, которые нужно решить в плане

1. Как создать и управлять пулом `GenericDmaVideoFrame`?
2. Как организовать кэширование wgpu textures для decoded frames?
3. Как интегрировать event-based API cros-codecs в существующий sync decode loop?
4. Как обрабатывать `FormatChanged` events и смену разрешения?
5. Как организовать graceful fallback если VA-API недоступен?
6. Какие acceptance criteria для каждого subtask?

### Acceptance Criteria для полного плана

- [ ] План содержит список всех файлов, которые нужно создать/изменить
- [ ] План содержит примерные оценки времени для каждого этапа
- [ ] План учитывает graceful degradation и error handling
- [ ] План включает стратегию тестирования (как проверить что hardware decode работает)
- [ ] План одобрен и понятен (можно приступать к реализации)

## Дополнительный контекст

### Почему не raw libva

Raw `libva` FFI требует ручного управления `VADisplay`, `VAConfig`, `VAContext`, `VASurfaceID`, ручного построения `VAPictureParameterBufferVP9`, `VASliceParameterBufferVP9`, ручного экспорта через `vaExportSurfaceHandle`. `cros-codecs` оборачивает всё это в safe Rust API.

### Почему CPU map приемлем

Intel UHD 620 — integrated GPU без dedicated VRAM. CPU и GPU используют shared system memory. DMA-BUF — это shared memory pages. `mmap()` даёт прямой доступ к той же физической памяти. Bandwidth DDR4 dual-channel ~25 GB/s, 4K60 NV12 = ~750 MB/s. CPU map overhead минимален.

### Existing render pipeline

`Nv12VideoRenderer` ожидает два `TextureView`:
- Y plane: `R8Unorm`, aspect `Plane0`
- UV plane: `Rg8Unorm`, aspect `Plane1`

Или можно использовать один NV12 texture с multi-planar views.

### Logging/telemetry

Используется `tracing` crate. Важные события:
- `info!` — инициализация decoder, открытие файла
- `debug!` — decode events, frame ready, format changed
- `warn!` — decode errors, skipped packets
- `info!` в telemetry — frame counters, backend name
