# 04. Codecs and Capabilities

## Основная политика

Видео воспроизводится только через аппаратный decode.

Если stream требует codec/profile/bit depth/chroma/HDR режим, который текущий hardware backend не поддерживает, stream считается unavailable. Плеер не включает software video fallback.

## Codec roadmap

Порядок развития:

1. Полный VP9
2. AV1
3. H.264
4. H.265
5. VP8

Этот порядок не запрещает раньше добавить probing для других codec'ов. Capability model должен быть шире, чем текущая реализация decode.

## Capability scan

Capability scan должен запускаться при старте или по запросу UI.

Для Linux/VA-API scan должен определить:

- VA-API доступен или нет;
- driver backend: i965, iHD или другой;
- vendor/device information;
- supported profiles;
- entrypoints;
- RT formats;
- bit depths;
- chroma formats;
- maximum coded/display resolution, если backend сообщает;
- known driver quirks;
- supported export/upload path;
- HDR metadata возможности, если доступны.

Результат scan должен быть типизированным, а не набором строк из `vainfo`.

## Модель данных

Пример целевых типов:

```rust
enum VideoCodec {
    Vp9,
    Av1,
    H264,
    H265,
    Vp8,
}

enum BitDepth {
    Eight,
    Ten,
    Twelve,
}

enum ChromaSubsampling {
    Yuv420,
    Yuv422,
    Yuv444,
}

enum TransferFunction {
    Bt709,
    Srgb,
    Pq,
    Hlg,
}

struct SupportedVideoDecodeFormat {
    codec: VideoCodec,
    profile: VideoProfile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    max_width: Option<u32>,
    max_height: Option<u32>,
    max_fps: Option<f64>,
    hdr_input: bool,
    backend: DecodeBackendId,
}
```

## Stream selection

Stream selection должен использовать capability matrix.

Алгоритм выбора:

1. Получить список candidate streams из контейнера или сервиса.
2. Нормализовать codec/profile/bit depth/chroma/HDR metadata.
3. Отфильтровать все stream'ы, которые hardware backend не поддерживает.
4. Отфильтровать stream'ы, которые renderer не может показать.
5. Выбрать лучший stream по политике качества, сети и настройкам.
6. Если stream не найден, вернуть понятную ошибку с reason list.

Пример user-facing ошибки:

```text
Не найден аппаратно поддерживаемый видеопоток.
Система поддерживает: H.264 Main/High 8-bit 4:2:0 до 1080p60.
Видео требует: AV1 Main10 4:2:0 HDR 2160p60.
Software fallback для видео отключен политикой rustiplayer.
```

## VA-API backend

Linux primary backend.

Нужно поддерживать оба драйверных направления:

- i965 для старых Intel;
- iHD для новых Intel.

Для старых устройств важен H.264 hardware decode. Пример целевой legacy-системы: Intel первого поколения с hardware H.264 и простым SDR playback.

## Decoder backend registry

Backend registry должен быть compile-time модульным.

Динамические плагины не используются. Все backend'ы компилируются в бинарь, но в коде они регистрируются как отдельные providers.

```rust
trait VideoDecodeBackendProvider {
    fn backend_id(&self) -> DecodeBackendId;
    fn probe(&self) -> anyhow::Result<BackendCapabilities>;
    fn create_decoder(&self, request: DecoderRequest) -> anyhow::Result<Box<dyn VideoDecoder>>;
}
```

## HDR stages

### Stage 1: HDR input to SDR output

Первая цель:

- принять HDR stream;
- сохранить color metadata;
- декодировать аппаратно;
- сделать tone mapping в shader;
- вывести SDR BT.709 на обычный монитор.

### Stage 2: Renderer color pipeline

Дальше:

- LUT/curve options;
- tone mapping presets;
- correct limited/full handling;
- diagnostic overlay для color path.

### Stage 3: Real HDR output

Позже:

- OS/compositor HDR support;
- swapchain format selection;
- HDR metadata propagation;
- per-platform implementation.

## Test matrix

Тестовая матрица должна покрывать:

- codec;
- profile;
- bit depth;
- chroma;
- HDR/SDR;
- resolution;
- FPS;
- container;
- expected hardware support;
- expected renderer path.

Пример manifest:

```toml
[[sample]]
id = "vp9-profile0-1080p60-sdr-webm"
path = "test-assets/video/vp9/profile0/1080p60-sdr.webm"
container = "webm"
codec = "vp9"
profile = "profile0"
bit_depth = 8
chroma = "yuv420"
hdr = false
width = 1920
height = 1080
fps = 60.0

[sample.expected]
linux_vaapi_i965 = "maybe"
linux_vaapi_ihd = "yes"
render_wgpu = "yes"
render_gles = "yes"
```

## Важные правила

- Capability scan должен быть доступен в UI.
- Capability scan должен быть логируемым.
- Capability scan должен быть тестируемым без запуска playback.
- Stream selection не должен выбирать неподдерживаемый поток "на авось".
- Renderer capabilities так же важны, как decoder capabilities.

