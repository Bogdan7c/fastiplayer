# Native YouTube Player — Architecture & Risk-Driven Design

**Дата актуализации:** 2026-05-04 (обновлено: переход с Vulkan Video на VA-API)  
**Статус:** исследовательский прототип / инженерный план  
**Основная цель:** нативный лёгкий YouTube/WebM/VP9-плеер без Electron, без браузерного движка и без FFmpeg в production decode path.  
**Ключевая гипотеза:** если убрать браузерный стек и использовать hardware VP9 decode через VA-API, можно получить более низкую загрузку CPU, меньше RAM и стабильнее frame pacing на слабом железе.

---

## 1. Концепция

Нативный онлайн-видеоплеер для YouTube-потоков с упором на:

- Rust-first архитектуру;
- **аппаратное VP9-декодирование через VA-API** (primary backend);
- отсутствие Electron/Chromium/WebView;
- отсутствие FFmpeg в финальном горячем пути;
- минимальное число копий decoded video frames;
- прямой рендер видео через `wgpu`/Vulkan + лёгкий UI-оверлей.

Проект не должен начинаться как «полноценная замена браузера». Правильная первая цель — доказать узкий pipeline:

```text
local.webm VP9/Opus
  → WebM demux
  → VA-API VP9 decode
  → NV12 GPU surface
  → CPU map (integrated GPU — shared memory, почти zero-cost)
  → wgpu texture upload
  → GPU color conversion
  → swapchain
  → synced audio
```

Только после стабильного локального файла имеет смысл добавлять YouTube extraction/streaming.

---

## 2. Что изменено относительно первоначального плана

### Ключевое изменение: Vulkan Video → VA-API

**2026-05-04 — Критическое открытие:** `VK_KHR_video_decode_vp9` **не доступен** на Intel UHD 620 (Kaby Lake, 8-е поколение). `vulkaninfo` показывает **ноль** video extensions. Попытка создания `VkVideoSessionKHR` приводит к panic из-за null function pointer (`create_video_session_khr`).

**Причина:** Vulkan Video decode поддерживается в драйвере Mesa/ANV начиная с **Intel Iris Xe** (Tiger Lake, 11-е поколение). Kaby Lake имеет аппаратный VP9 decode блок, но драйвер ANV не экспортирует его через Vulkan Video API.

**Решение:** Переход на **VA-API** как primary backend для hardware decode.

### Исправлено

- ✅ `symphonia` больше не указан как VP9-видеодекодер. Это аудио/мультимедиа-крейт, пригодный для Opus/аудио, но не для software VP9 video decode.
- ✅ "Zero-copy от сети до экрана" заменено на более точное: **decoded video near-zero-copy path** (для integrated GPU CPU map практически бесплатен из-за shared memory).
- ✅ **Vulkan Video VP9 backend отменён** как primary. Код `video-vulkan` сохранён для reference, но не используется на целевом железе.
- ✅ **VA-API стал primary backend**. Crate `video-vaapi` будет реализован через `cros-codecs` (ChromeOS hardware codec library).
- ✅ `gpu-video` указан как полезная база/референс для Vulkan Video + `wgpu`, но неактуален для текущего железа.
- ✅ YouTube extraction сделан заменяемым адаптером, потому что внутренние API YouTube могут ломаться.
- ✅ Производительность переведена из обещаний в измеряемые целевые метрики.

---

## 3. Non-goals

На первых этапах проект **не** пытается:

- обходить DRM, платный доступ, региональные ограничения или access control;
- реализовывать полноценный YouTube-клиент с логином, комментариями и рекомендациями;
- поддерживать все кодеки YouTube сразу;
- поддерживать все контейнеры;
- конкурировать с `mpv` по универсальности;
- быть production-ready с первого MVP;
- гарантировать одинаковую работу на всех GPU и драйверах.

Первый production-worthy target: **публичные WebM VP9 + Opus потоки без DRM и без обхода ограничений доступа**.

---

## 4. Результаты Hardware & Driver Gate (перепроверены 2026-05-04)

### Целевое железо

```text
ThinkPad T480s
Intel UHD 620 / Kaby Lake Refresh (8-е поколение)
Linux / Mesa / ANV / i965 VA-API driver
```

### Результаты проверки

**Vulkan Video:**
```bash
$ vulkaninfo | grep -i "VK_KHR_video"
# (no output — zero video extensions)
```

**VA-API:**
```bash
$ vainfo | grep -i "vp9"
      VAProfileVP9Profile0            :	VAEntrypointVLD
      VAProfileVP9Profile0            :	VAEntrypointEncSlice
      VAProfileVP9Profile2            :	VAEntrypointVLD
```

**Вывод:**
- ❌ **Vulkan Video VP9 недоступен** на Intel UHD 620 через ANV
- ✅ **VA-API VP9 decode доступен** через i965 driver
- ✅ **VA-API VP9 encode доступен** (бонус)

### Важное различие

Наличие аппаратного VP9-блока через **VA-API** **≠** наличие `VK_KHR_video_decode_vp9` через **Vulkan Video**. Это разные API paths:

| API | Статус на UHD 620 | Поколение Intel |
|---|---|---|
| VA-API VP9 decode | ✅ Доступен | Sandy Bridge+ |
| Vulkan Video VP9 | ❌ Недоступен | Tiger Lake (Xe)+ |

---

## 5. Архитектура верхнего уровня

```text
┌────────────────────────────────────────────────────────────┐
│                      Player App                            │
│               egui / winit / wgpu / tracing                │
└────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────┐
│                     Player Core                            │
│ state machine, clocks, buffering, seek, quality selection  │
└─────────────┬──────────────────────┬──────────────────────┘
              │                      │
              ▼                      ▼
┌──────────────────────┐   ┌────────────────────────────────┐
│ Source Adapter       │   │ Local File Adapter              │
│ YouTube/Innertube    │   │ .webm test files                │
│ stream URL / ranges  │   │ deterministic debugging         │
└──────────┬───────────┘   └───────────────┬────────────────┘
            │                               │
            └──────────────┬────────────────┘
                          ▼
┌────────────────────────────────────────────────────────────┐
│                   WebM / Matroska Demux                    │
│ VP9 packets + PTS/DTS + Opus packets + timestamps          │
└─────────────┬────────────────────────────┬────────────────┘
              │                            │
              ▼                            ▼
┌─────────────────────────────┐   ┌──────────────────────────┐
│ Video Pipeline              │   │ Audio Pipeline            │
│ VA-API VP9 decode           │   │ Symphonia Opus            │
│ (cros-codecs)               │   │ ringbuf → CPAL            │
│ NV12 DMA-BUF surface        │   │ audio clock               │
│ CPU map → wgpu texture      │   │                           │
│ GPU color conversion        │   │                           │
│ swapchain presentation      │   │                           │
└─────────────────────────────┘   └──────────────────────────┘
```

### Почему CPU map приемлем для integrated GPU

Intel UHD 620 — **integrated GPU** без выделенной видеопамяти. CPU и GPU используют одну системную RAM через shared memory architecture.

```text
NV12 4K frame: 3840 × 2160 × 1.5 = ~12.4 MB
At 60 fps: 12.4 × 60 = ~744 MB/s
DDR4 dual-channel bandwidth: ~25 GB/s
CPU map overhead: <3% of memory bandwidth
```

**Для integrated GPU CPU map практически бесплатен**, потому что:
1. Нет PCIe transfer (GPU не имеет dedicated VRAM)
2. DMA-BUF — это просто shared memory pages
3. `mmap()` даёт прямой доступ к той же физической памяти

---

## 6. VA-API Decode Pipeline (Новый Primary Backend)

### Компоненты

```text
┌─────────────────────────────────────────────────────────┐
│                VA-API VP9 Decode Pipeline                │
├─────────────────────────────────────────────────────────┤
│  cros-codecs::StatelessDecoder<Vp9, VaapiBackend>       │
│  ├── VP9 bitstream parser (codec::vp9::parser)          │
│  ├── VA-API context (libva::Display + Config + Context) │
│  ├── VA-API surfaces (NV12, DMA-BUF backed)             │
│  └── Decode commands (vaBeginPicture/vaRenderPicture)   │
│                                                          │
│  Output: DecodedHandle<GenericDmaVideoFrame>             │
│  ├── DMA-BUF file descriptors                            │
│  ├── DRM format: NV12                                    │
│  └── Resolution: stream-dependent                        │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────┐
│  Frame Export                                            │
│  ├── handle.sync() — ждём завершения decode             │
│  ├── handle.video_frame().map() — mmap DMA-BUF          │
│  └── Получаем &[u8] slices для Y и UV плоскостей        │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────┐
│  wgpu Upload                                             │
│  ├── queue.write_texture() → Y plane texture            │
│  ├── queue.write_texture() → UV plane texture           │
│  └── Nv12VideoRenderer → fullscreen quad shader         │
└─────────────────────────────────────────────────────────┘
```

### Почему cros-codecs

**`cros-codecs`** (ChromeOS) — production-ready crate для hardware-accelerated codecs на Linux:

- ✅ VA-API decoder для VP9 (проверено на ChromeOS/crosvm)
- ✅ Поддержка H.264, H.265, VP8, VP9, AV1
- ✅ DMA-BUF export через `GenericDmaVideoFrame`
- ✅ Stateless decoder API — мы управляем frame pool и input/output
- ✅ Разработан Google для ChromeOS, зрелый код

Зависимости:
```toml
[dependencies]
cros-codecs = { version = "0.0.6", features = ["vaapi"] }
```

### Почему не raw libva

Raw `libva` FFI требует:
- Ручного управления `VADisplay`, `VAConfig`, `VAContext`, `VASurfaceID`
- Ручного построения `VAPictureParameterBufferVP9`, `VASliceParameterBufferVP9`
- Ручного экспорта через `vaExportSurfaceHandle` + `VADRMPRIMESurfaceDescriptor`
- Сложного mapping reference frames

`cros-codecs` оборачивает всё это в safe Rust API с типажами `StatelessDecoderBackend` и `VideoFrame`.

---

## 7. Zero-copy vs Near-zero-copy для Integrated GPU

### Оригинальная цель (Vulkan Video)

```text
encoded VP9 packet in RAM
    │
    ▼
Vulkan Video decode command
    │
    ▼
VkImage / NV12 / GPU memory
    │
    ▼
GPU shader: NV12 → RGB/RGBA or direct YUV sampling
    │
    ▼
wgpu render pass / swapchain
```

Этот путь **недостижим** на Intel UHD 620 из-за отсутствия Vulkan Video.

### Новый путь (VA-API + CPU map)

```text
encoded VP9 packet in RAM
    │
    ▼
VA-API decode (GPU video engine)
    │
    ▼
NV12 DMA-BUF surface (shared system memory)
    │
    ▼
CPU mmap (zero-copy для integrated GPU)
    │
    ▼
wgpu::Queue::write_texture() (CPU → GPU texture, но на том же чипе RAM)
    │
    ▼
GPU shader: NV12 → RGB
    │
    ▼
wgpu render pass / swapchain
```

**Ключевое отличие:** для integrated GPU шаг "CPU mmap → write_texture" — это не "копия из CPU RAM в GPU VRAM", а "копия внутри shared system memory". Bandwidth позволяет 4K60 без проблем.

### Near-zero-copy contract

Проект считается успешным по decoded video path, если:

- ✅ GPU video engine выполняет VP9 decode (не CPU)
- ✅ Нет CPU-side VP9 decode
- ✅ Нет intermediate RGB frame в RAM
- ✅ UI compositing не заставляет копировать full-frame video через CPU
- ⚠️ Есть одна CPU map для DMA-BUF (неизбежно для VA-API → wgpu без Vulkan external memory)

---

## 8. Компоненты

### 8.1 Source Adapter

Без изменений (см. оригинальный раздел 9.1).

### 8.2 HTTP streaming

Без изменений (см. оригинальный раздел 9.2).

### 8.3 WebM / Matroska demux

Без изменений (см. оригинальный раздел 9.3).

### 8.4 Video decode: VA-API VP9 (Новый primary backend)

**Текущая реализация:** `video-vaapi` crate через `cros-codecs`.

Архитектура:

```rust
use cros_codecs::decoder::stateless::vp9::StatelessDecoder;
use cros_codecs::decoder::stateless::StatelessVideoDecoder;
use cros_codecs::decoder::BlockingMode;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;

// Создание decoder
let display = libva::Display::open().unwrap();
let decoder = StatelessDecoder::<Vp9, VaapiBackend<GenericDmaVideoFrame>>
    ::new_vaapi(display, BlockingMode::Blocking)?;
```

Decode loop:

```rust
// 1. Парсим VP9 header через vp9-parser
let frame_info = vp9_parser::parse_uncompressed_header(&packet.data)?;

// 2. Передаём bitstream в cros-codecs decoder
let size = decoder.decode(
    pts.as_micros() as u64,
    &packet.data,
    &mut alloc_callback,  // предоставляем output buffer из pool
)?;

// 3. Получаем готовые кадры через events
while let Some(event) = decoder.next_event() {
    match event {
        DecoderEvent::FrameReady(handle) => {
            // Синхронизируемся с GPU
            handle.sync()?;
            
            // Получаем доступ к frame memory
            let frame = handle.video_frame();
            let mapping = frame.map()?;
            let planes = mapping.get();  // Vec<&[u8]>
            
            // planes[0] = Y, planes[1] = UV (interleaved)
            // Upload в wgpu texture через queue.write_texture()
        }
        DecoderEvent::FormatChanged => {
            // Пересоздаём wgpu textures под новый размер
        }
    }
}
```

**Frame Pool Management:**

`StatelessDecoder` требует, чтобы caller предоставлял output buffers через `alloc_cb`. Мы создаём пул `GenericDmaVideoFrame` заранее:

```rust
// Создаём DMA-BUF backed frames через GBM или dma-heap
let frame_pool: Vec<GenericDmaVideoFrame> = create_dma_frame_pool(
    width, height,
    cros_codecs::DecodedFormat::NV12,
    num_frames,
)?;
```

**Интеграция с `VideoDecoder` trait:**

```rust
pub struct VaapiVideoDecoder {
    decoder: DynStatelessVideoDecoder<GenericDmaVideoFrame>,
    frame_pool: Vec<GenericDmaVideoFrame>,
    pending_outputs: VecDeque<DecodedFrame>,
    // ...
}

impl VideoDecoder for VaapiVideoDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>> {
        // Submit bitstream
        // Collect ready frames from event queue
        // Return oldest ready frame
    }
}
```

### 8.5 VP9 parser

Без изменений (см. оригинальный раздел 9.5). `vp9-parser` всё ещё нужен для keyframe detection в demuxer.

### 8.6 Color conversion / render

Без изменений (см. оригинальный раздел 9.6). `Nv12VideoRenderer` принимает Y/UV plane views и конвертирует в RGB через WGSL shader.

### 8.7 UI

Без изменений (см. оригинальный раздел 9.7).

### 8.8 Audio

Без изменений (см. оригинальный раздел 9.8).

---

## 9. Обновлённая структура репозитория

```text
native-yt-player/
  Cargo.toml                          # workspace
  Cargo.lock                          # lockfile
  youtube-player-architecture-updated.md  # этот документ
  AGENTS.md                           # инструкции для агента
  crates/
    app-egui/                         # Stage 1+2+2.5+3: приложение
      Cargo.toml
      src/
        main.rs                       # entry point, winit ApplicationHandler, render_frame()
        render.rs                     # wgpu device/swapchain, synthetic + video render pipeline
        state.rs                      # egui UI, player state, hotkeys, file dialog, video pipeline init
        telemetry.rs                  # AtomicU64 счётчики FPS/frames/packets/video frames
        shaders/
          mod.rs                      # модуль шейдеров
          synthetic.rs                # WGSL шейдер цветных полос (Stage 1)
          nv12_to_rgba.wgsl          # NV12 → RGBA конверсия
    webm-demux/                       # Stage 2: WebM/Matroska demux
      Cargo.toml
      src/
        lib.rs
        demuxer.rs                    # Demuxer trait
        symphonia_demuxer.rs          # Symphonia-based WebM demuxer + VP9 keyframe detection
        packet.rs                     # Packet, TrackInfo, TrackKind, TimeBase
        error.rs                      # DemuxError
    audio/                            # Stage 2: Audio pipeline
      Cargo.toml
      src/
        lib.rs
        decoder.rs                    # OpusDecoder (opus crate wrapper)
        output.rs                     # AudioOutput (cpal + ringbuf)
        clock.rs                      # AudioClock (thread-safe sample counters)
    vp9-parser/                       # Stage 2.5: VP9 bitstream parser
      Cargo.toml
      src/
        lib.rs
        bit_reader.rs                 # Big-endian bit reader (MSB first)
        uncompressed_header.rs          # VP9 uncompressed header parser
    video-core/                       # Stage 2.5: video decode abstractions + A/V sync
      Cargo.toml
      src/
        lib.rs
        decoder.rs                    # VideoDecoder trait, DecodedFrame, FrameTextureHandle, ColorSpace
        sync.rs                       # AvSync, FrameAction
    video-vaapi/                      # NEW Stage 3: VA-API primary backend
      Cargo.toml
      src/
        lib.rs
        decoder.rs                    # VaapiVideoDecoder — impl VideoDecoder via cros-codecs
        frame_pool.rs                 # DMA-BUF frame pool management
    video-vulkan/                     # Stage 3 (DEPRECATED на UHD 620): Vulkan Video backend
      Cargo.toml
      src/
        lib.rs                        # Сохранено для reference и future GPU
        instance.rs
        device.rs
        decoder.rs
        frame_texture.rs
        session.rs
        dpb.rs
        decode.rs
        vp9.rs
        raw_video.rs
    render/                           # Stage 3C: video rendering
      Cargo.toml
      src/
        lib.rs                        # VideoRenderer trait
        nv12_renderer.rs              # Nv12VideoRenderer — NV12→RGB shader
      shaders/
        nv12_to_rgba.wgsl             # NV12 → RGBA WGSL shader
  test-assets/
    README.md
    download.sh                       # скачивание тестовых семплов
    test.webm                         # fallback тестовый файл
    1080p/
      big_buck_bunny_1080p.webm       # VP9/Opus, 1920x1080, ~10s
    480p/
      # (пусто — 480p URL вернул 404)
  docs/
    superpowers/                      # контекст для агента
    plans/                            # implementation plans
  target/                             # cargo build artifacts
```

---

## 10. Обновлённый стек технологий

| Слой | Кандидат | Статус | Версия |
|---|---|---|---|
| UI/window | `egui`, `winit`, `wgpu` | ✅ Подтверждено для MVP | egui 0.34.1, winit 0.30.13, wgpu 29.0.3 |
| VA-API bindings | `cros-codecs` + `libva` | ✅ Новый primary backend | cros-codecs 0.0.6 |
| VP9 parser | `vp9-parser` (internal) | ✅ Реализован (Stage 2.5) | — |
| Video decode core | `video-core` (internal) | ✅ Реализован (Stage 2.5) | — |
| Video VA-API backend | `video-vaapi` (internal) | 🆕 **Новый primary** (Stage 3) | — |
| Video Vulkan backend | `video-vulkan` (internal) | ⚠️ Deprecated на UHD 620, сохранён для reference | — |
| Video renderer | `render` (internal) | ✅ NV12→RGB shader с letterboxing (Stage 3C) | — |
| HTTP | `reqwest` | ⏳ Подходит | — |
| WebM demux | `symphonia` | ✅ Работает для local file | 0.5.5 |
| Audio decode | `opus` crate (libopus) | ✅ Работает | 0.3.1 |
| Audio output | `cpal` | ✅ Работает | 0.15 |
| Buffers | `ringbuf`, `bytes` | ✅ Подходит | ringbuf 0.4.8, bytes 1.x |
| Logging | `tracing`, `tracing-subscriber` | ✅ Обязательно | tracing 0.1, subscriber 0.3 |
| Metrics | custom counters (`AtomicU64`) | ✅ Работает | — |
| Build helpers | `pollster`, `bytemuck` | ✅ Для blocking async и POD casts | pollster 0.4, bytemuck 1.25 |
| File dialog | `rfd` | ✅ Работает | 0.15 |

---

## 11. Roadmap с acceptance criteria (обновлён)

### Этап 0 — Hardware/driver gate ✅ ЗАВЕРШЁН

**Дата:** 2026-05-04 (перепроверен)

- [x] Проверить `vulkaninfo` — **нет video extensions**.
- [x] Проверить VA-API через `vainfo` — **VP9 Profile0/Profile2 decode доступен**.
- [x] Зафиксировать решение: переход с Vulkan Video на VA-API.

**Acceptance:** понятно, что делать на текущем железе. ✅

**Результат:**
- ❌ `VK_KHR_video_decode_vp9` **не доступен** на Intel UHD 620 (ANV, Mesa).
- ✅ VA-API baseline **подтверждён** — `VAProfileVP9Profile0` + `VAProfileVP9Profile2`.
- ✅ Основная дорожная карта **меняется**: VA-API становится primary backend.

---

### Этап 1 — Render shell без видеодекодера ✅ ЗАВЕРШЁН

Без изменений (см. оригинальный раздел).

---

### Этап 2 — Local WebM demux + audio clock ✅ ЗАВЕРШЁН

Без изменений (см. оригинальный раздел).

---

### Этап 2.5 — Подготовка video decode pipeline ✅ ЗАВЕРШЁН

Без изменений (см. оригинальный раздел).

---

### Этап 3 — Local VA-API VP9 decode 🆕 ОБНОВЛЁН

**Оценка:** 1–2 недели.

**Принятые архитектурные решения:**
1. **Primary backend:** VA-API через `cros-codecs` crate (ChromeOS).
2. **Frame export:** `GenericDmaVideoFrame` → DMA-BUF → CPU mmap → `queue.write_texture()`.
3. **Integrated GPU optimization:** CPU map приемлем из-за shared memory architecture.
4. **Fallback:** если VA-API недоступен — сообщение "No hardware VP9 decode available".
5. **Vulkan Video code:** сохранён в `video-vulkan` для future GPU, но не используется на UHD 620.

---

#### 3A: VA-API Decoder Integration (~3–5 дней)

**Цель:** интегрировать `cros-codecs` VA-API VP9 decoder в проект.

**Файлы:** `crates/video-vaapi/Cargo.toml`, `crates/video-vaapi/src/decoder.rs`, `crates/video-vaapi/src/frame_pool.rs`

**Задачи:**
- [ ] Добавить `cros-codecs` зависимость с `vaapi` feature.
- [ ] Создать `VaapiVideoDecoder` — impl `VideoDecoder` trait.
- [ ] Интегрировать `StatelessDecoder<Vp9, VaapiBackend<GenericDmaVideoFrame>>`:
  - `libva::Display::open()` для Wayland/X11.
  - `StatelessDecoder::new_vaapi(display, BlockingMode::Blocking)`.
- [ ] Реализовать frame pool через `GenericDmaVideoFrame`:
  - Создать 12 NV12 DMA-BUF backed frames.
  - Предоставлять их через `alloc_cb` в `decode()`.
- [ ] Обработка `DecoderEvent::FrameReady`:
  - `handle.sync()` для синхронизации.
  - `handle.video_frame().map()` для получения `&[u8]` slices.
  - Конвертация в `DecodedFrame` с `FrameTextureHandle`.
- [ ] Обработка `DecoderEvent::FormatChanged`:
  - Пересоздание wgpu textures под новый размер.
- [ ] Graceful fallback: если `Display::open()` fails или decoder creation fails — вернуть ошибку.

---

#### 3B: NV12 Upload + Rendering (~2–3 дня)

**Цель:** загружать decoded NV12 frames в wgpu и рендерить через существующий `Nv12VideoRenderer`.

**Файлы:** `crates/video-vaapi/src/decoder.rs`, `crates/app-egui/src/main.rs`

**Задачи:**
- [ ] Создать два `wgpu::Texture` (Y plane + UV plane) или один NV12 texture.
- [ ] `queue.write_texture()` для Y plane (`R8Unorm`).
- [ ] `queue.write_texture()` для UV plane (`Rg8Unorm` или interleaved NV12).
- [ ] Передать `TextureView` в `Nv12VideoRenderer`.
- [ ] Интеграция в `app-egui::render_frame()`:
  - Downcast `VideoDecoder` → `VaapiVideoDecoder`.
  - Получить Y/UV views для текущего кадра.
  - Render через `Nv12VideoRenderer`.

---

#### 3C: Интеграция и тестирование (~2–3 дня)

**Цель:** стабильное воспроизведение локального VP9/Opus WebM.

**Задачи:**
- [ ] Wire `VaapiVideoDecoder` в `app-egui` video loop.
- [ ] Тест с `test-assets/*.webm`:
  - 1080p60 VP9 Profile 0
  - 1440p60 VP9 Profile 0
- [ ] Verify hardware decode: `intel_gpu_top` показывает video engine load.
- [ ] Verify CPU load: < 10% для 1080p60.
- [ ] Graceful degradation:
  - VA-API нет → понятная ошибка "No hardware VP9 decode available".
- [ ] Performance telemetry: decode time per frame.

**Acceptance:** локальный VP9/Opus WebM играет с реальным видео и звуком, GPU video engine активен.

---

### Этап 4 — YouTube streaming adapter

Без изменений (см. оригинальный раздел).

---

### Этап 5 — Player features

Без изменений (см. оригинальный раздел).

---

### Этап 6 — Качество картинки и оптимизация

Без изменений (см. оригинальный раздел).

---

## 12. Performance model (обновлён)

### Целевые метрики для T480s / UHD 620

| Метрика | MVP target | Как измерять |
|---|---|---|
| 1080p60 VP9 CPU | < 10% process CPU | `top`, `perf`, `pidstat` |
| 4K60 VP9 CPU | < 15% process CPU | `pidstat`, `perf` |
| Dropped frames | < 0.5% после прогрева | internal frame counter |
| RAM | < 150 MB для MVP | `/proc/<pid>/status`, `smem` |
| Cold start | < 1 s | internal timing |
| Audio drift | < 20 ms steady-state | internal clock telemetry |
| GPU video engine | decode engine active | `intel_gpu_top` |
| Power | ниже браузерного baseline | `powertop`, battery discharge logs |

**Примечание:** CPU target выше чем для Vulkan Video (где ожидалось <5%), потому что VA-API path включает CPU map + `write_texture()`. Для integrated GPU это всё равно низкая нагрузка.

---

## 13. Риски (обновлённые)

| Риск | Вероятность | Урон | Митигировать так |
|---|---|---|---|
| Vulkan VP9 не доступен на UHD 620 через ANV | ✅ **Подтверждено** | — | **Переход на VA-API** |
| VA-API нестабилен на i965 driver | Низкая | Средний | `cros-codecs` — production-tested на ChromeOS; fallback на software decode для debug |
| cros-codecs не собирается на target системе | Низкая | Высокий | Проверить заранее через `cargo check`; `libva` dev headers должны быть установлены |
| DMA-BUF map → wgpu upload bandwidth | Низкая | Средний | Для integrated GPU — shared memory, bandwidth ~25 GB/s; 4K60 = ~750 MB/s |
| Frame pool starvation (stateless decoder) | Средняя | Средний | Alloc 12+ frames; return frames to pool после render |
| wgpu interop с external DMA-BUF | Высокая | Высокий | **Не используем** — вместо этого CPU map + write_texture |
| YouTube adapter ломается | Высокая | Средний | Source abstraction; local file MVP; graceful failure |
| A/V sync сложнее ожиданий | ✅ Решено (Stage 2.5) | — | `AvSync` + `FrameAction` реализованы в `video-core` |
| Производительность не лучше mpv | Средняя | Средний | Сравнивать честно; оптимизировать только после работающего pipeline |
| HDR/цвета выглядят неправильно | Средняя | Средний | Сначала SDR-only; explicit color metadata |
| Mesa ANV VP9 decode баги (если вернёмся к Vulkan Video) | Н/Д | — | Не актуально — используем VA-API |

---

## 14. Первый реальный milestone (обновлён)

```text
M1: Local VP9/Opus WebM player via VA-API

- Запускается на T480s. ✅
- Показывает локальный 1080p60 VP9 файл.
- Звук синхронный.
- Есть frame/drop counters. ✅
- Decode path использует VA-API hardware decode.
- GPU video engine активен (intel_gpu_top).
```

**Этап 1 завершён (2026-05-02):** Render shell с synthetic video, egui overlay, telemetry, VSync.

**Этап 2 завершён (2026-05-03):** Local WebM demux + Opus decode + CPAL output + audio clock + telemetry.

**Текущий статус (2026-05-04):**
- ✅ Звук проигрывается из локального VP9/Opus WebM.
- ✅ Audio clock работает.
- ✅ UI показывает telemetry.
- ✅ Keyframe detection через `vp9-parser`.
- ✅ A/V sync реализован.
- ✅ NV12 → RGB rendering работает через `Nv12VideoRenderer`.
- ❌ **Vulkan Video VP9 decode отменён** — не работает на UHD 620.
- 🆕 **VA-API primary backend** — `vainfo` подтверждает VP9 decode.
- ⏳ `video-vaapi` crate — не начат, требуется интеграция `cros-codecs`.

---

## 15. Итоговая формулировка проекта (обновлена)

**Native YouTube Player** — это не "магический плеер без узких мест", а рискованный, но технически осмысленный Rust/Vulkan/VA-API эксперимент:

```text
Минимизировать browser overhead,
использовать hardware VP9 decode через VA-API на Linux,
держать video pipeline узким и измеримым,
рендерить через wgpu с минимальными копиями (integrated GPU).
```

Главная инженерная ставка:

```text
Если VA-API VP9 decode доступен на целевом драйвере,
а cros-codecs предоставляет production-ready Rust API,
то нативный плеер может быть легче браузера на слабом железе
с hardware decode, а не CPU fallback.
```

Главная инженерная неопределённость (устранена):

```text
❌ Vulkan Video VP9 зрелость на старых Intel GPU
✅ VA-API VP9 доступен и зрел на i965 driver
```

---

## 16. Ссылки для проверки

- `cros-codecs` crate/docs  
  https://crates.io/crates/cros-codecs  
  https://docs.rs/cros-codecs

- `cros-libva` crate (dependency of cros-codecs)  
  https://crates.io/crates/cros-libva

- VA-API specification  
  https://intel.github.io/libva/

- Mesa i965 driver documentation  
  https://docs.mesa3d.org/drivers/i965.html

- `vainfo` / `libva-utils`  
  https://github.com/intel/libva-utils

- Khronos: Vulkan Video Decode VP9 Extension (reference only)  
  https://www.khronos.org/blog/khronos-announces-vulkan-video-decode-vp9-extension

- Mesa 25.2.0 release notes  
  https://docs.mesa3d.org/relnotes/25.2.0.html

- Phoronix: Intel ANV VP9 Vulkan Video support merged for Mesa 25.2  
  https://www.phoronix.com/news/Intel-ANV-VP9-Vulkan-Video

- `symphonia` docs  
  https://docs.rs/symphonia

- `cpal` docs  
  https://docs.rs/cpal/latest/cpal/

- `rustube` crate/docs  
  https://crates.io/crates/rustube  
  https://docs.rs/rustube
