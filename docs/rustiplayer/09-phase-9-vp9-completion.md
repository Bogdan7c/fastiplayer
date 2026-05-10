# 09. Phase 9 Full VP9 Completion

## Цель

Phase 9 закрывает VP9 как codec/capability направление перед HDR-to-SDR.

Задача этапа - превратить текущий рабочий VP9 Profile 0 SDR MVP в полную typed VP9 модель:

- распознавать все VP9 profiles;
- точно определять profile/bit depth/chroma/resolution из надёжных источников;
- честно отделять поддержанные hardware/render paths от recognized-but-unsupported вариантов;
- сохранить текущий SDR VP9/NV12 playback без визуальной регрессии;
- подготовить codec-agnostic `P010 + HDR metadata + zero-copy render boundary` для Phase 10.

Phase 9 не реализует HDR tone mapping и не показывает HDR как washed-out SDR. HDR playback в production path остаётся rejected до Phase 10.

## Reference checks

Перед планированием Phase 9 были проверены актуальные внешние reference-точки:

- Context7 по `wgpu` 29: texture creation, bind groups, uniform binding size, runtime feature checks;
- `wgpu` docs: `TextureFormat::P010`, `R8/Rg8/R16/Rg16`, `TextureAspect::Plane0/Plane1`, feature-gated texture formats;
- WebM VP codec binding: VP9 codec string содержит profile/level/bit depth/chroma/color fields;
- Matroska/WebM Colour elements: transfer characteristics, primaries, matrix coefficients, range;
- VP9 bitstream header: profile, bit depth, chroma subsampling и color config;
- VA-API RT formats: `YUV420`, `YUV420_10`, `YUV420_12`, профиль VP9 Profile 2.

Reference links:

- [`wgpu::TextureFormat`](https://docs.rs/wgpu/latest/wgpu/enum.TextureFormat.html);
- [`wgpu::TextureAspect`](https://docs.rs/wgpu/latest/wgpu/enum.TextureAspect.html);
- [WebM VP codec binding](https://www.webmproject.org/vp9/mp4/);
- [Matroska elements](https://www.matroska.org/technical/elements.html);
- [VP9 bitstream header PDF](https://downloads.webmproject.org/docs/vp9/vp9-bitstream_superframe-and-uncompressed-header_v1.0.pdf).

Перед каждой implementation-сессией нужно снова свериться с Context7 по внешним crate-ам, которые реально трогаются в этой сессии.

## Принятые решения

### Scope VP9 completion

Выбран вариант полной typed модели при поддержке только доказанных playback paths.

Phase 9 должна распознавать весь VP9 surface, но playable считаются только варианты, которые одновременно поддержаны hardware decoder-ом, renderer boundary и текущей production policy.

```text
VP9 Profile 0 8-bit 4:2:0 SDR -> supported production path, NV12
VP9 Profile 1 8-bit 4:2:2/4:4:4 -> recognized, rejected
VP9 Profile 2 10-bit 4:2:0 -> supported decode/P010 boundary if hardware allows
VP9 Profile 2 12-bit 4:2:0 -> recognized, rejected
VP9 Profile 3 10/12-bit 4:2:2/4:4:4 -> recognized, rejected
```

Это считается full VP9 completion для rustiplayer, потому что unsupported варианты не игнорируются и не превращаются в ложный hardware/backend error.

### Phase 9 и Phase 10 boundary

Phase 9 отдаёт Phase 10 codec-agnostic контракт:

```text
DecodedFrame {
  format = P010,
  bit_depth = 10,
  chroma = YUV420,
  color = BT.2020 PQ/HLG limited/full,
  memory_path = DmaBufZeroCopy,
}
```

VP9 является первым producer-ом этого контракта. AV1/H.265 позже должны подключиться к тому же path без переписывания HDR renderer-а.

### P010 support state

Не вводим простой boolean `supports_p010_input`, потому что он может случайно разрешить P010 production playback до появления P010 shader-а.

Нужен typed state или два явных поля:

```rust
enum P010RenderReadiness {
    Unavailable,
    ZeroCopyBoundaryVerified,
    Renderable,
}
```

Phase 9 может дойти только до `ZeroCopyBoundaryVerified`.

Production stream selection не должна считать HDR/P010 playable, пока Phase 10 не переведёт renderer в `Renderable` и не включит `supports_hdr_to_sdr`.

### VP9 12-bit

VP9 12-bit распознаётся и отклоняется.

Phase 9/10 не делают `P012` или 12-bit shader/import path. Причина rejection должна быть typed:

```text
UnsupportedBitDepth { codec: VP9, bit_depth: 12 }
```

### VP9 Profile 1/3

VP9 Profile 1/3 распознаются и отклоняются из-за 4:2:2/4:4:4 chroma.

Причина rejection должна быть typed:

```text
UnsupportedChroma { codec: VP9, chroma: YUV422/YUV444 }
```

Software decode, CPU conversion или временный RGB fallback не добавляются.

### Metadata resolver

Используем layered resolver с field-level precedence:

```text
service manifest / vp09 codec string
  -> container track Colour elements
  -> VP9 bitstream header
  -> decoder/backend output format
  -> fallback only for SDR unknown
```

Precedence:

- profile/bit depth/chroma/resolution: confirmed bitstream сильнее manifest/container hints;
- transfer/primaries/matrix/range: container Colour может быть сильнее VP9 header, если header не выражает поле точно;
- decoded pixel format/bit depth/chroma: decoder/backend сильнее всех для фактического output;
- conflicts не замалчиваются, а попадают в typed diagnostics.

Для HDR strict core metadata обязательна:

```text
transfer = PQ или HLG
primaries = BT.2020
matrix = BT.2020
range = Limited или Full
bit_depth = 10
chroma = YUV420
format = P010
```

Если после resolution metadata этих полей нет или они конфликтуют, HDR stream не считается playable.

### Production HDR policy

Phase 9 не включает HDR playback для пользователя.

Допустимый результат Phase 9:

```text
VP9 Profile2 10-bit HDR decode capability = available
P010 zero-copy boundary = verified
HDR-to-SDR renderer = unavailable until Phase 10
production HDR playback = rejected with clear reason
```

Недопустимо:

- показывать HDR как SDR fallback;
- делать CPU P010 upload/readback fallback;
- объявлять `supports_hdr_to_sdr = true`;
- включать washed-out debug rendering в production path.

### Test assets

Используем два уровня тестов:

1. Small fixtures in repo:
   - golden VP9 headers;
   - metadata TOML/JSON fixtures;
   - conflict cases;
   - capability/rejection cases.
2. External/manual real samples:
   - VP9 Profile 0 SDR WebM regression sample;
   - VP9 Profile 2 10-bit HDR WebM sample.

Большие media files не нужно коммитить в repo, но manual test policy должна фиксировать expected logs и expected capability state.

## Non-goals

- Не реализовывать BT.2446, tone mapping или HDR shader.
- Не объявлять HDR-to-SDR support.
- Не делать native HDR output.
- Не добавлять P012/12-bit rendering.
- Не поддерживать VP9 4:2:2/4:4:4 playback.
- Не добавлять software video decode fallback.
- Не добавлять CPU color conversion/readback ради P010.
- Не переносить AV1/H.265 backend implementation в Phase 9.
- Не менять намеренно текущий SDR VP9/NV12 visual result.

## Архитектура

```text
service/source metadata
  vp09 codec string, manifest hints
        |
        v
demux-webm/media-core
  track metadata, Matroska/WebM Colour elements
        |
        v
codec-core
  VP9 profile/bit depth/chroma/metadata model
  VP9 color metadata resolver
        |
        v
capability-core
  decode + render + device capability intersection
  typed rejection reasons
        |
        v
video-vaapi
  VP9 Profile0 NV12 production decode
  VP9 Profile2 P010 zero-copy boundary readiness
        |
        v
video-core
  DecodedFrame { format, bit_depth, chroma, color, memory_path, texture_handle }
        |
        v
render-wgpu
  WgpuFramePlanes::Nv12 production path
  WgpuFramePlanes::P010 boundary diagnostic path
        |
        v
app-egui
  diagnostics only, no codec/color business logic
```

## Target VP9 matrix

| VP9 variant | Поведение Phase 9 | Причина |
| --- | --- | --- |
| Profile 0, 8-bit, 4:2:0, SDR | Production playback via NV12 | Current MVP path, protected by regression tests |
| Profile 0, unknown metadata | Soft SDR BT.709 fallback with diagnostics | Existing fallback policy from Phase 8.5 |
| Profile 2, 10-bit, 4:2:0, HDR | Decode/P010 boundary readiness, production playback rejected до Phase 10 | HDR renderer ещё не реализован |
| Profile 2, 10-bit, 4:2:0, SDR | Decode/P010 boundary recognized, production playback только если renderer говорит `Renderable` | P010 не равен HDR |
| Profile 2, 12-bit, 4:2:0 | Rejected | P012/12-bit path out of scope |
| Profile 1, 8-bit, 4:2:2/4:4:4 | Rejected | Chroma unsupported |
| Profile 3, 10/12-bit, 4:2:2/4:4:4 | Rejected | Chroma and/or bit depth unsupported |

## Типы и контракты

### `codec-core`

Добавить или уточнить:

- `Vp9StreamRequirement`;
- conversion из VP9 bitstream profile/bit depth/chroma в `VideoDecodeRequirement`;
- typed `VideoRequirementRejection`;
- `HdrMetadataField`;
- `ColorMetadataConflict`;
- helper для strict HDR core metadata validation.

`VideoDecodeRequirement` должен уметь выразить:

- codec;
- profile;
- bit depth;
- chroma;
- resolution;
- HDR flag;
- resolved color metadata или ссылку на resolved color metadata summary.

### `media-core` / `webm-demux`

Track metadata должна уметь передать WebM/Matroska Colour elements без потери:

- matrix coefficients;
- bits per channel, если есть;
- chroma subsampling hints, если есть;
- color range;
- transfer characteristics;
- primaries;
- mastering metadata;
- MaxCLL/MaxFALL, если контейнер их сообщил.

### `capability-core`

Selection должна использовать intersection:

```text
decode backend capability
  + renderer production capability
  + renderer/import boundary capability
  + device features
  + strict metadata validation
```

Phase 9 important rule:

- `P010RenderReadiness::ZeroCopyBoundaryVerified` достаточно для ручной dev-readiness проверки;
- для production playback требуется `P010RenderReadiness::Renderable`;
- `supports_hdr_to_sdr` остаётся `false`.

### `video-core`

`DecodedFrame` должен стать format-aware:

```rust
enum DecodedPixelFormat {
    Nv12,
    P010,
}

enum FrameMemoryPath {
    DmaBufZeroCopy,
    CpuUpload,
}

struct DecodedFrame {
    format: DecodedPixelFormat,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    color: VideoColorMetadata,
    memory_path: FrameMemoryPath,
    texture_handle: FrameTextureHandle,
}
```

Production rule:

- `NV12` может использовать текущий production path;
- `P010` в Phase 9 допускается только как zero-copy boundary diagnostic;
- `P010 + CpuUpload` запрещён.

### `render-wgpu`

Расширить WGPU wrapper:

```rust
enum WgpuFramePlanes<'frame> {
    Nv12 {
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    },
    P010 {
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    },
}
```

В Phase 9 `P010` planes используются только для readiness validation/manual diagnostics. Production render path для P010 появляется в Phase 10.

## Новый VP9 data flow

1. Service/container даёт ранние VP9 hints.
2. VP9 bitstream probe уточняет profile/bit depth/chroma/resolution.
3. Metadata resolver объединяет manifest/container/bitstream/backend fields.
4. Capability layer строит requirement и typed rejection reasons.
5. SDR Profile 0 идёт через существующий NV12 production path.
6. HDR Profile 2 10-bit идёт в manual readiness path:
   - VA-API выдаёт P010/I010 surface;
   - surface экспортируется как DMA-BUF;
   - WGPU импортирует P010 zero-copy;
   - создаётся `WgpuFramePlanes::P010`;
   - renderer boundary логирует verified state;
   - production playback возвращает контролируемый unsupported HDR renderer reason.

## Декомпозиция по сессиям

### Сессия 1: VP9 requirement matrix

Статус: реализовано

Задачи:

- расширить VP9 parser adapter до complete profile/bit-depth/chroma requirement model;
- добавить recognized states для Profile 0/1/2/3;
- добавить explicit 12-bit и 4:2:2/4:4:4 rejection cases;
- убедиться, что parse uncertainty не становится fatal reject.

Unit tests:

- Profile 0 8-bit 4:2:0 -> `NV12` requirement;
- Profile 2 10-bit 4:2:0 -> `P010` candidate requirement;
- Profile 2 12-bit -> `UnsupportedBitDepth(12)`;
- Profile 1/3 -> `UnsupportedChroma`;
- incomplete/non-keyframe packet -> recoverable/no strict reject.

Manual tests:

- current VP9 SDR sample still starts;
- unsupported profile sample/header gives exact diagnostic, not `HardwareDecoderUnavailable`.

### Сессия 2: VP9/WebM color metadata resolver

Задачи:

- добавить WebM/Matroska Colour extraction или adapter над существующими track metadata;
- добавить resolver field-level precedence;
- добавить conflict diagnostics;
- добавить strict HDR core metadata validation.

Unit tests:

- container `BT.2020 PQ limited` + Profile 2 10-bit -> strict HDR core valid;
- container `BT.2020 HLG limited` + Profile 2 10-bit -> strict HDR core valid;
- missing transfer for HDR candidate -> typed missing metadata rejection;
- bitstream profile conflict with container hint -> bitstream wins for profile;
- container/bitstream color conflict -> conflict diagnostic recorded.

Manual tests:

- inspect media info panel/logs for resolved VP9 SDR metadata;
- inspect HDR sample logs for `BT.2020 PQ/HLG` fields.

### Сессия 3: capability and rejection reason model

Задачи:

- ввести codec-agnostic typed rejection reasons;
- обновить capability selection для decode/render/device intersection;
- развести `P010 zero-copy boundary` и `P010 renderable`;
- убедиться, что UI не содержит codec-specific selection logic.

Unit tests:

- Profile 1/3 rejected as unsupported chroma;
- 12-bit rejected as unsupported bit depth;
- VP9 Profile 2 10-bit HDR rejected in production because HDR renderer unavailable;
- P010 boundary verified state alone does not make stream playable;
- reason formatter produces user-facing Russian explanation.

Manual tests:

- capability report shows VP9 Profile 2 decode if VA-API supports it;
- production HDR stream selection explains missing HDR renderer until Phase 10.

### Сессия 4: decoded frame and WGPU frame contract

Задачи:

- добавить `format`, `bit_depth`, `chroma`, `memory_path` в `DecodedFrame`;
- обновить existing NV12 callers;
- добавить `WgpuFramePlanes::P010`;
- не ломать scheduler/frame queue;
- сохранить renderer-neutral boundary.

Unit tests:

- NV12 decoded test frame содержит `format=Nv12`, `bit_depth=8`, `memory_path` expected;
- P010 boundary frame содержит `format=P010`, `bit_depth=10`, `memory_path=DmaBufZeroCopy`;
- `P010 + CpuUpload` rejected by validation;
- scheduler tests не завязаны на конкретный pixel format.

Manual tests:

- SDR VP9/NV12 playback работает;
- active color path Phase 8.5 не изменился.

### Сессия 5: VA-API VP9 P010 output and zero-copy boundary

Задачи:

- поддержать VA-API Profile 2 10-bit output surface allocation;
- корректно маппить `I010/P010` decoded format в `P010` frame contract;
- добавить P010 DMA-BUF import path без CPU fallback;
- создать P010 plane views;
- логировать first P010 descriptor и zero-copy readiness.

Unit tests:

- VA RT format `YUV420_10` -> `P010` decoded contract;
- P010 import requires `TEXTURE_FORMAT_P010`;
- P010 boundary rejects missing zero-copy importer;
- imported P010 views use plane 0/plane 1 and 16-bit plane formats.

Manual tests:

- real VP9 Profile 2 HDR sample reaches P010 boundary;
- logs show `P010 zero-copy boundary verified`;
- no CPU upload/readback log appears for P010;
- when P010 feature is missing, failure is clear and controlled.

### Сессия 6: test matrix and manual diagnostic mode

Задачи:

- добавить golden VP9 header fixtures;
- добавить metadata fixtures;
- добавить manual diagnostic mode for P010 boundary;
- документировать external sample policy.

Unit tests:

- all golden headers parse to expected requirements;
- metadata fixtures resolve to expected fields;
- conflict fixture records expected conflicts;
- production HDR remains rejected until Phase 10.

Manual tests:

- `RUSTIPLAYER_DEV_VERIFY_P010_BOUNDARY=1` with VP9 HDR sample;
- expected final diagnostic:

```text
P010 zero-copy boundary verified: VP9 Profile2 10-bit BT.2020 PQ/HLG YUV420
HDR-to-SDR renderer unavailable until Phase 10
```

### Сессия 7: SDR regression, self-review and docs

Задачи:

- проверить текущий SDR VP9/NV12 path после всех VP9 changes;
- проверить, что fallback paths не игнорируют ошибки молча;
- проверить, что names не стали абстрактными вроде `data/temp/obj`;
- проверить, что `app-egui` не содержит codec/color selection logic;
- обновить docs, если реализация уточнила детали.

Verification:

- `cargo fmt`;
- `cargo check`;
- targeted tests по `codec-core`, `capability-core`, `webm-demux`, `video-core`, `video-vaapi`, `render-wgpu`, `player-core`;
- manual VP9 SDR playback;
- manual VP9 HDR P010 boundary diagnostic;
- capability report before/after comparison.

## Acceptance checklist

- VP9 Profile 0 SDR production playback работает.
- SDR visual result не менялся намеренно.
- VP9 Profile 0/1/2/3 распознаются.
- VP9 12-bit распознаётся и rejected как unsupported bit depth.
- VP9 4:2:2/4:4:4 распознаётся и rejected как unsupported chroma.
- VP9 Profile 2 10-bit 4:2:0 может дойти до P010 zero-copy boundary на поддержанном hardware.
- Production HDR playback всё ещё rejected до Phase 10.
- P010 path не имеет CPU upload/readback fallback.
- Layered metadata resolver покрыт conflict tests.
- Strict HDR core metadata validation покрыта tests.
- Capability rejection reasons typed и codec-agnostic.
- UI не содержит VP9-specific business logic.
- External manual HDR sample policy задокументирована.

## Как это готовит Phase 10

После Phase 9 Phase 10 сможет заниматься только HDR renderer/tone mapping:

- `DecodedFrame` уже несёт pixel format, bit depth, chroma, color metadata и memory path;
- P010 zero-copy import уже проверен на реальном VP9 HDR sample;
- strict HDR core metadata уже валидируется до render;
- unsupported VP9 variants уже дают точные reasons;
- SDR VP9/NV12 path защищён regression tests;
- AV1/H.265 смогут стать новыми producers того же P010/HDR контракта.
