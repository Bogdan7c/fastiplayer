# 04. Codecs and Capabilities

## Основная политика

Видео воспроизводится только через аппаратный decode.

Если stream требует codec/profile/bit depth/chroma/HDR режим, который текущий hardware backend не поддерживает, stream считается unavailable. Плеер не включает software video fallback.

## Текущая production поддержка

Поддержка означает прохождение полного capability intersection: decoder backend,
DMA-BUF export/import, renderer import/render path и color pipeline. Если хотя бы
одна часть не проходит, stream должен получить typed reject до тяжелого playback
path или во время fail-closed backend boundary.

- VP9 Profile 0, 8-bit, 4:2:0, SDR: `NV12` через VA-API hardware decode,
  DMA-BUF zero-copy export/import и SDR BT.709 renderer path.
- VP9 Profile 2, 10-bit, 4:2:0, HDR PQ/HLG: `P010` через VA-API hardware decode,
  DMA-BUF zero-copy export/import, strict HDR metadata и BT.2446-C HDR-to-SDR
  renderer path.

VP9/HDR stream с неполной container metadata может быть предварительно выбран как
candidate, но окончательное разрешение на decode даётся только после codec
bitstream refinement и production capability check. Runtime env shim
`RUSTIPLAYER_DEV_VERIFY_P010_BOUNDARY` после smooth playback Session 9 удалён и
не является допустимым способом обойти renderer/HDR gate.

## Known unsupported formats

Следующие варианты не являются production support и не должны рекламироваться как
доступные:

- software video decode;
- CPU upload или CPU readback decoded video frames;
- VP9 12-bit;
- VP9 4:2:2 и 4:4:4;
- VP9 Profile 1 и Profile 3 для текущего renderer/backend path;
- P010 BT.2020 SDR или wide-gamut SDR без отдельного gamut mapping path;
- native HDR output в swapchain/OS compositor;
- AV1, H.264, H.265 и VP8 production decode, пока для них не добавлены codec
  adapters, backend validation и renderer/capability integration.

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

## Bitstream probing policy

Capability matrix описывает, что система умеет декодировать. Stream requirement описывает, что требует конкретный поток. Requirement можно уточнять из трёх источников:

1. service manifest: codec string, profile, resolution, HDR, bitrate/FPS;
2. container metadata: codec id, codec private data, coded/display dimensions;
3. codec bitstream headers: VP9 uncompressed header, AV1 sequence header OBU, H.264 SPS, H.265 VPS/SPS.

Правила probing:

- сначала использовать metadata из manifest/container;
- bitstream parser запускать только как уточнение, а не как единственный источник истины;
- сверять bitstream resolution с container/display metadata, если оба источника доступны;
- fatal reject делать только когда parser успешно прочитал валидный header и typed requirement точно не проходит capability matrix;
- parse error, неполный packet, non-keyframe без нужного header'а или неизвестное поле не должны становиться `HardwareDecoderUnavailable`;
- при неуверенном результате нужно логировать diagnostic/recoverable событие и продолжать decode path;
- parser output с невозможными значениями, например нулевой размер или размер сильно больше container metadata, считается invalid probe result и не используется для отказа.

Нельзя добавлять новые самописные bit-level parser'ы в `player-core` ради быстрого capability check. Для будущих codec'ов нужно использовать parser, который уже применяет decode backend, или тонкий adapter над ним:

- VP9: текущий `vp9-parser` допустим только как минимальный header adapter, покрытый golden tests и сверенный с parser order из `cros-codecs`;
- AV1: использовать `cros-codecs` AV1 sequence header parser или adapter над ним;
- H.264: использовать `cros-codecs` H.264 SPS parser или adapter над ним;
- H.265: использовать `cros-codecs` H.265 VPS/SPS parser или adapter над ним;
- VP8: не добавлять strict probing без реальной необходимости, потому что profile/format matrix проще и обычно определяется backend capability.

Цель этого правила - не блокировать воспроизводимый поток из-за ошибки предварительного parser'а. Capability probing должен уменьшать число поздних decoder errors, но не заменять decoder validation неполным дубликатом.

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

enum ColorRange {
    Limited,
    Full,
    Unknown,
}

enum MatrixCoefficients {
    Bt601,
    Bt709,
    Bt2020,
    Unknown,
}

enum ColorPrimaries {
    Bt709,
    Bt2020,
    Smpte170m,
    Bt470Bg,
    Unknown,
}

enum TransferFunction {
    Bt709,
    Srgb,
    Pq,
    Hlg,
    Unknown,
}

enum ColorMetadataOrigin {
    FallbackDefault,
    Manifest,
    Container,
    Bitstream,
    DecoderBackend,
}

enum ColorMetadataConfidence {
    Fallback,
    Hint,
    Confirmed,
}

struct VideoColorMetadata {
    range: ColorRange,
    matrix: MatrixCoefficients,
    primaries: ColorPrimaries,
    transfer: TransferFunction,
    hdr_metadata: Option<HdrMetadata>,
    origin: ColorMetadataOrigin,
    confidence: ColorMetadataConfidence,
}

enum VideoSurfaceFormat {
    Nv12,
    P010,
    Rgba8,
}

enum ZeroCopyExportRequirement {
    DmaBuf,
}

enum VideoMemoryContract {
    HardwareZeroCopy {
        export: ZeroCopyExportRequirement,
    },
}

struct ColorPipelineRequirement {
    requires_hdr_processing: bool,
    metadata_origin: Option<ColorMetadataOrigin>,
    metadata_confidence: Option<ColorMetadataConfidence>,
}

struct FrameTimingContract {
    nominal_frame_rate: Option<f64>,
}

struct VideoDecodeRequirement {
    codec: VideoCodec,
    profile: Option<VideoProfile>,
    bit_depth: Option<BitDepth>,
    chroma: Option<ChromaSubsampling>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
    surface_format: Option<VideoSurfaceFormat>,
    memory_contract: VideoMemoryContract,
    color_pipeline: ColorPipelineRequirement,
    timing_contract: FrameTimingContract,
    hdr: bool,
    color: Option<VideoColorMetadata>,
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

`VideoColorMetadata::sdr_bt709_limited()` является явным fallback default для текущего VP9/NV12 SDR path. Этот helper не должен маскировать источник metadata: diagnostics должны отличать fallback от metadata, прочитанной из manifest/container/bitstream/decoder.

`VideoDecodeRequirement` является codec-neutral контрактом. Codec adapters отвечают за profile/level/bit-depth/chroma validation, codec private/header parsing, surface format selection и confidence/origin color metadata. Capability layer дальше делает intersection только по общим полям: decoder backend capabilities, renderer import/render capabilities, color pipeline capabilities, platform restrictions и mandatory zero-copy memory contract.

Новый codec добавляется через adapter, который возвращает `VideoRequirementProbe::{Candidate, Rejected, Recoverable}`. `Rejected` используется только для валидного header-а с подтверждённо неподдерживаемым variant-ом; `Recoverable` не должен запускать ложный hardware failure. `player-core` не должен импортировать codec-specific parser crate или делать profile branching.

## Color metadata resolution

Color metadata собирается layered-моделью:

1. service manifest даёт ранний hint для stream selection;
2. container metadata уточняет track-level fields, если они есть;
3. codec bitstream parser подтверждает codec-specific colorimetry;
4. decoder/backend подтверждает фактический decoded pixel format, bit depth и chroma;
5. fallback default применяется только если metadata не была получена надёжно.

Правило конфликтов: bitstream metadata сильнее manifest/container, если parser успешно прочитал валидный header. Decoder/backend сильнее всех для фактического decoded surface format, но не обязан быть единственным источником colorimetry. Если источники конфликтуют, player должен логировать diagnostic note и использовать наиболее надёжный источник без ложного `HardwareDecoderUnavailable`.

Confidence policy:

- `Fallback` - значение выбрано default-ом, потому что metadata не была получена;
- `Hint` - значение пришло из manifest/container и пригодно для предварительного выбора;
- `Confirmed` - значение подтверждено bitstream parser-ом или decoder/backend-ом.

## Stream selection

Stream selection должен использовать capability matrix.

Алгоритм выбора:

1. Получить список candidate streams из контейнера или сервиса.
2. Нормализовать codec/profile/bit depth/chroma/HDR metadata.
3. Отфильтровать все stream'ы, которые hardware backend не поддерживает.
4. Отфильтровать stream'ы, которые renderer не может показать.
5. Выбрать лучший stream по политике качества, сети и настройкам.
6. Если stream не найден, вернуть понятную ошибку с reason list.

Если точная metadata появляется только после первого keyframe/header packet, selection должен уметь делать два этапа:

1. предварительный выбор по codec/container/service metadata;
2. уточнение requirement перед decode с мягкой probing policy из раздела выше.

Второй этап не должен уничтожать выбранный stream при неуверенности parser'а. Отказ допустим только при подтверждённо неподдерживаемом profile/format/resolution/HDR.

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

Текущий статус после Phase 10: Stage 0, Stage 0.5 и Stage 1 закрыты для VP9/P010 baseline. Production HDR-to-SDR доступен только через аппаратный P010 zero-copy path, strict HDR metadata и renderer capability intersection. Stage 2 advanced quality modes/LUT presets и Stage 3 native HDR output остаются future work.

### Stage 0: SDR color pipeline prep

Закрыто Phase 8.5: текущие SDR assumptions вынесены из shader-а в явный renderer contract:

- сохранить `NV12 BT.709 limited -> SDR` визуально как раньше;
- передавать range/matrix/adjustments через uniforms;
- добавить active color path diagnostics;
- на этапе Phase 8.5 не объявлять HDR support и не добавлять P010 renderer преждевременно.

### Stage 0.5: Full VP9 completion before HDR

Закрыто Phase 9: VP9 стал первым реальным producer P010/HDR контракта:

- распознавать VP9 Profile 0/1/2/3;
- поддержать текущий Profile 0 SDR/NV12 production path;
- распознавать и честно отклонять 12-bit, 4:2:2 и 4:4:4 VP9 variants;
- добавить VP9/WebM layered color metadata resolver;
- доказать `P010 + HDR metadata + zero-copy` render boundary для VP9 Profile 2 10-bit 4:2:0;
- на этапе Phase 9 не показывать HDR до появления Phase 10 tone mapping.

### Stage 1: HDR input to SDR output

Закрыто Phase 10 для VP9/P010 baseline:

- принять HDR stream;
- использовать color metadata и P010 boundary из VP9 completion;
- декодировать аппаратно без software fallback;
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
- color range;
- matrix coefficients;
- color primaries;
- transfer function;
- color metadata origin/confidence;
- resolution;
- FPS;
- container;
- expected hardware support;
- expected renderer path.
- expected probing behavior: confirmed support, confirmed rejection или мягкий fallback probing.
- expected active color path.

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
