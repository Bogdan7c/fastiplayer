# 10. Phase 10 HDR-to-SDR Baseline

## Цель

Phase 10 добавляет качественный HDR-to-SDR output поверх готового Phase 9 VP9/P010 контракта.

Задача этапа - принять `P010 10-bit YUV420` HDR frame с typed color metadata, выполнить HDR-to-SDR conversion по ITU-R BT.2446 Method C и вывести SDR BT.709 на обычный SDR monitor.

Phase 10 не чинит VP9 parser/decode. Если Phase 9 не доказала `P010 + HDR metadata + zero-copy render boundary`, Phase 10 начинать нельзя.

## Prerequisites from Phase 9

Перед реализацией Phase 10 должны быть закрыты:

- VP9 Profile 0 SDR regression path;
- VP9 Profile 2 10-bit 4:2:0 requirement detection;
- strict HDR core metadata validation;
- `DecodedFrame { format=P010, bit_depth=10, chroma=YUV420, color=BT.2020 PQ/HLG, memory_path=DmaBufZeroCopy }`;
- P010 DMA-BUF zero-copy import boundary with separate-layer `R16Unorm + Rg16Unorm` plane views as the baseline storage;
- composed P010 zero-copy import kept only as a compatibility storage layout;
- typed capability/rejection reasons;
- production HDR playback rejected until HDR renderer is implemented.

## Reference checks

Перед планированием Phase 10 были проверены актуальные внешние reference-точки:

- Context7 по `wgpu` 29: texture creation, bind group layouts, uniforms, feature checks;
- `wgpu` docs: `TextureFormat::P010`, `TextureAspect::Plane0/Plane1`, `TEXTURE_FORMAT_P010`, `TEXTURE_FORMAT_16BIT_NORM`, `R16Unorm`, `Rg16Unorm`;
- ITU-R BT.2446: HDR-to-SDR conversion methods, выбран Method C;
- SMPTE ST 2084 / PQ и HLG transfer model через BT.2100-compatible references;
- Matroska/WebM Colour metadata fields для PQ/HLG/BT.2020.

Reference links:

- [`wgpu::TextureFormat`](https://docs.rs/wgpu/latest/wgpu/enum.TextureFormat.html);
- [`wgpu::TextureAspect`](https://docs.rs/wgpu/latest/wgpu/enum.TextureAspect.html);
- [ITU-R BT.2446-1](https://www.itu.int/pub/R-REP-BT.2446-1-2021);
- [Matroska elements](https://www.matroska.org/technical/elements.html).

Перед каждой implementation-сессией нужно снова свериться с Context7 по внешним crate-ам, которые реально трогаются в этой сессии.

## Принятые решения

### P010 input

Phase 10 принимает P010 только через zero-copy.

Запрещено:

- CPU upload/readback fallback для P010;
- временный RGB/RGBA intermediate ради обхода P010 import;
- показ HDR как SDR fallback.

P010 import capability зависит от фактического VA-API export layout:

```text
baseline separate-layer P010 export -> требует TEXTURE_FORMAT_16BIT_NORM и plane textures R16Unorm/Rg16Unorm
compat composed P010 export         -> требует TEXTURE_FORMAT_P010
```

Если zero-copy import для фактически экспортированного layout недоступен, HDR stream не выбирается. Phase 10 считает separate-layer `R16Unorm + Rg16Unorm` plane views (DRM `R16`/`GR1616`) основным P010 storage layout, потому что именно этот path проверен на Intel i965. Composed `TextureFormat::P010` остаётся совместимостью для драйверов, где он реально работает.

### Tone mapping

Выбран ITU-R BT.2446 Method C.

Не используем Reinhard, ACES fitted или simple scale/clamp как production baseline. Эти операторы могут появиться только как отдельная будущая задача, если будет принято новое решение.

### PQ и HLG

Phase 10 поддерживает PQ и HLG сразу:

```text
P010 + PQ  -> PQ EOTF  -> HDR linear/display light -> BT.2446 Method C
P010 + HLG -> HLG EOTF -> HDR linear/display light -> BT.2446 Method C
```

Transfer decode и tone mapping разделяются в коде, но используют общий HDR-to-SDR pipeline после перехода в HDR light domain.

### Output transfer

HDR path использует `ExplicitShaderOetf`.

Shader формирует SDR BT.709 output values и пишет их в текущий `Unorm` swapchain target. SDR NV12 path из Phase 8.5 остаётся в `PreserveCurrentUnorm`.

Diagnostics example:

```text
P010 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf
```

### BT.2020 to BT.709

BT.2020 -> BT.709 делается как часть BT.2446 Method C.

Не используем простой `BT.2020 RGB -> BT.709 RGB + clamp` shortcut как production behavior.

### Metadata policy

Strict core HDR metadata обязательна:

```text
format = P010
bit_depth = 10
chroma = YUV420
transfer = PQ или HLG
primaries = BT.2020
matrix = BT.2020
range = Limited или Full
```

Optional metadata:

- mastering display max/min luminance;
- MaxCLL;
- MaxFALL.

Если optional metadata отсутствует, Phase 10 использует documented reference defaults и явно показывает это в diagnostics. Missing optional metadata не должна маскироваться как confirmed stream metadata.

### Shader structure

SDR и HDR shader paths разделены.

`nv12_to_rgba.wgsl` остаётся SDR/NV12 shader path.

Добавляется отдельный HDR shader:

```text
render-wgpu/shaders/p010_bt2446c_to_sdr.wgsl
```

Внутри shader file допускается один fullscreen pass, но код должен быть разбит на понятные функции:

- P010 sampling;
- range normalization;
- YUV BT.2020 -> RGB;
- PQ/HLG EOTF;
- BT.2446 Method C;
- SDR BT.709 output transfer;
- final clamp.

Не вводим custom WGSL preprocessor/import system в Phase 10.

Так как `P010` не равен `HDR`, P010 SDR BT.709 не должен проходить через BT.2446-C. Допустимы два production-safe варианта реализации:

- P010 renderer имеет отдельный SDR branch внутри P010 shader module;
- P010 renderer добавляет небольшой sibling shader для `P010 10-bit SDR -> SDR BT.709`.

В обоих случаях `nv12_to_rgba.wgsl` не превращается в HDR/P010 shader.

P010 renderer получает уже нормализованный renderer boundary:

```text
WgpuFramePlanes::P010 { y_view, uv_view }
```

Renderer не должен зависеть от того, создана ли эта пара views из baseline separate-layer textures `R16Unorm + Rg16Unorm` или из compatibility composed `TextureFormat::P010` texture. Storage layout остаётся обязанностью `video-vaapi`/texture cache; shader/bind group работают с plane views.

### P010 не равно HDR

`P010` - pixel format, а не dynamic range.

Phase 10 выбирает path по metadata:

```text
P010 + PQ/HLG -> HDR-to-SDR BT.2446-C
P010 + BT.709 SDR -> 10-bit SDR YUV->SDR BT.709 path
P010 + BT.2020 SDR -> supported only after explicit gamut mapping decision, otherwise typed reject
```

Все P010 кадры нельзя безусловно гонять через HDR tone mapping.

Если P010 SDR BT.709 кадр несёт side metadata с non-HDR transfer, это не делает
кадр HDR. Такой кадр остаётся в P010 SDR branch и не получает reference-default
HDR diagnostics. HDR branch выбирается только когда основная colorimetry или
согласованная side metadata реально требуют PQ/HLG processing.

### Config и UI

Phase 10 добавляет typed HDR-to-SDR config, но не добавляет tone mapping presets в UI.

Config defaults:

```toml
[render.hdr_to_sdr]
enabled = true
operator = "bt2446_c"
sdr_reference_white_nits = 100.0
hdr_reference_peak_nits = 1000.0
```

Важное правило migration: старый scalar placeholder `render.hdr_to_sdr = false` из Phase 8.5 нельзя хранить одновременно с таблицей `[render.hdr_to_sdr]`. Phase 10 читает старые scalar fields только как compatibility input, а default/persisted format держит в таблице `[render.hdr_to_sdr]`.

UI показывает diagnostics:

- active color path;
- transfer;
- primaries;
- matrix;
- reference defaults, если они использованы;
- native HDR output unsupported.

UI не содержит color math и не даёт пользователю выбрать Reinhard/ACES в Phase 10.

### Native HDR output

Native HDR output остаётся future contract.

Phase 10:

```text
supports_hdr_to_sdr = true
supports_native_hdr_output = false
HdrOutputMode = SdrBt709Only
```

Wayland/compositor/display HDR metadata не входят в scope.

### Error policy

Phase 10 fail-closed.

Правила:

- если capability intersection не проходит, stream не выбирается;
- если P010 frame пришёл, но decoder/importer не может экспортировать/import-ить P010 zero-copy, decoder thread отдаёт fatal error в `player-core`;
- если shell не может получить texture views для present frame через render-side
  provider или `WgpuRenderableFrame` отвергает decoded contract, `app-egui`
  отправляет typed `PlayerRenderError` в worker, а worker обновляет fatal media
  error snapshot;
- если renderer не может bind/render P010, это fatal media error;
- если strict HDR metadata invalid, stream rejected до render;
- device/surface lost обрабатывается текущей runtime recovery, но color path не деградирует silently;
- SDR/CPU fallback для HDR запрещён.

## Non-goals

- Не реализовывать native HDR output.
- Не делать P012/12-bit HDR.
- Не делать VP9 parser/decode completion.
- Не добавлять AV1/H.265 backend implementation.
- Не добавлять CPU P010 fallback.
- Не добавлять LUT infrastructure.
- Не добавлять UI presets для alternative tone mapping.
- Не менять намеренно текущий SDR NV12 result.
- Не превращать `nv12_to_rgba.wgsl` в HDR shader.

## Архитектура

```text
video-core
  DecodedFrame { P010, 10-bit, YUV420, BT.2020 PQ/HLG, DmaBufZeroCopy }
        |
        v
render-core
  HDR render capabilities
  ActiveColorPath with bt2446-c explicit-shader-oetf
  HdrToSdrSettings
        |
        v
render-wgpu
  P010VideoRenderer
  P010 plane bindings independent of composed/separate storage layout
  HdrColorPipelineUniforms
  p010_bt2446c_to_sdr.wgsl
        |
        v
swapchain
  SDR BT.709, Unorm, explicit shader OETF
        |
        v
app-egui
  diagnostics only
```

## Модули

### `render-core`

Добавить или уточнить:

- `HdrToSdrSettings`;
- `HdrToneMappingOperator::Bt2446C`;
- `HdrOutputMode::SdrBt709Only`;
- `P010RenderReadiness`;
- `ActiveColorPath` с operator/output transfer fields;
- capability fields для HDR-to-SDR и native HDR output.

### `render-wgpu`

Предлагаемая структура:

```text
src/
  color_pipeline/
    mod.rs
    sdr.rs
    hdr.rs
    transfer.rs
    bt2446c.rs
    metadata_validation.rs
    bt2446c_reference.rs   # только tests/reference
  nv12_renderer.rs
  p010_renderer.rs
shaders/
  nv12_to_rgba.wgsl
  p010_bt2446c_to_sdr.wgsl
```

`bt2446c_reference.rs` не должен быть runtime dependency path. Он нужен для deterministic tests и reference vectors.

### `config`

Добавить `[render.hdr_to_sdr]` typed config.

Validation:

- `operator` в Phase 10 только `bt2446_c`;
- `sdr_reference_white_nits > 0`;
- `hdr_reference_peak_nits >= sdr_reference_white_nits`;
- unreasonable values rejected или clamped only with explicit warning, без silent fallback.

### `app-egui`

UI только отображает:

- active HDR color path;
- operator;
- input metadata;
- output mode;
- reference-default markers;
- capability rejection reason.

UI не считает transfer functions, matrices или tone mapping.

## HDR data flow

1. Player выбирает HDR stream только если decode + P010 zero-copy + renderer HDR-to-SDR capability проходят intersection.
2. Decoder отдаёт `DecodedFrame` с `P010`, `10-bit`, `YUV420`, strict HDR metadata.
3. `WgpuRenderableFrame::from_decoded_p010` создаёт `RenderableFrame` и `WgpuFramePlanes::P010` из existing plane views, не делая assumptions о backing storage.
4. `WgpuVideoRenderer::render_or_clear` выбирает `P010VideoRenderer`.
5. `P010VideoRenderer` валидирует metadata и settings.
6. CPU-side color pipeline строит `HdrColorPipelineUniforms`.
7. Shader:
   - читает P010 Y/UV plane values;
   - нормализует 10-bit limited/full range;
   - выполняет BT.2020 YUV->RGB;
   - применяет PQ/HLG EOTF;
   - выполняет BT.2446 Method C;
   - формирует SDR BT.709 output values;
   - пишет в `Unorm` target.
8. `ActiveColorPath` попадает в diagnostics.

Если P010 export/import падает, `video-vaapi::VideoDecodeThread` передаёт
typed `DecodeThreadError` в `player-core`. `PlayerSession` переводит его в
fatal state и очищает pending video packets. Shell-side ошибки render boundary
также становятся fatal player errors, а не тихой заменой кадра на empty/black
video pass.

## Декомпозиция по сессиям

### Сессия 1: render-core HDR contracts/capabilities

Статус: реализовано

Задачи:

- добавить `HdrToSdrSettings`;
- добавить `HdrToneMappingOperator::Bt2446C`;
- добавить `HdrOutputMode::SdrBt709Only`;
- использовать существующий `P010RenderReadiness::Renderable` как состояние production-ready P010 renderer;
- обновить `RenderCapabilities`;
- обновить `ActiveColorPath` diagnostics.

Unit tests:

- HDR-to-SDR capabilities включаются только при P010 renderable + BT.2446-C;
- native HDR output остаётся false;
- `P010 zero-copy boundary` без renderer не делает stream playable;
- active path string содержит `bt2446-c explicit-shader-oetf`.

Manual tests:

- capability report показывает HDR-to-SDR unavailable до включения P010 renderer;
- SDR capability summary не регрессировал.

### Сессия 2: P010 renderer skeleton and binding

Статус: реализовано

Задачи:

- добавить `p010_renderer.rs`;
- использовать существующий `WgpuRenderableFrame::from_decoded_p010` boundary wrapper;
- создать bind group layout для P010 planes + HDR uniforms;
- выбрать P010 renderer только для `P010` frames;
- поддержать baseline separate-layer P010 bindings и не завязать renderer на compatibility composed storage;
- заложить отдельный metadata branch для `P010 SDR BT.709`, который не вызывает BT.2446-C;
- оставить `Nv12VideoRenderer` неизменным для SDR.

Unit tests:

- P010 frame dispatches to P010 renderer path;
- NV12 frame dispatches to NV12 renderer path;
- P010 renderer rejects non-zero-copy memory path;
- baseline separate-layer P010 и compatibility composed P010 дают одинаковый renderer-facing `WgpuFramePlanes::P010`;
- P010 SDR BT.709 dispatches to non-HDR P010 path;
- shader source file exists and is not `nv12_to_rgba.wgsl`.

Manual tests:

- SDR VP9/NV12 playback работает;
- Phase 9 manual P010 sample доходит до renderer dispatch без SDR fallback.

### Сессия 3: HDR uniforms and metadata validation

Статус: реализовано

Задачи:

- добавить `HdrColorPipelineUniforms`;
- обеспечить WGSL uniform layout alignment;
- добавить strict metadata validation;
- добавить reference-default markers для optional metadata;
- добавить range normalization constants для 10-bit limited/full P010.

Unit tests:

- uniform size/alignment/offsets совпадают с WGSL;
- PQ strict core metadata accepted;
- HLG strict core metadata accepted;
- missing transfer/primaries/matrix/range rejected;
- missing MaxCLL/MaxFALL uses reference-default marker, not confirmed metadata;
- P010 SDR BT.709 metadata accepted without HDR optional defaults;
- P010 limited range normalization uses 10-bit code values.

Manual tests:

- HDR sample logs show resolved strict core metadata;
- invalid metadata sample rejected before render.

### Сессия 4: BT.2446-C CPU reference implementation

Статус: реализовано

Задачи:

- добавить isolated Rust reference implementation for tests;
- покрыть PQ EOTF known points;
- покрыть HLG EOTF known points;
- покрыть BT.2020 YUV->RGB matrix reference samples;
- покрыть BT.2446-C reference vectors;
- покрыть SDR BT.709 output transfer.

Unit tests:

- PQ black/reference/peak points;
- HLG black/reference/peak points;
- matrix conversion known samples;
- BT.2446-C intermediate values stay finite;
- output values clamp only at final allowed stage;
- CPU reference rejects invalid metadata/settings.

Manual tests:

- runtime behavior в этой сессии не требуется;
- compare reference output against external tool/sample when available.

### Сессия 5: WGSL BT.2446-C shader implementation

Статус: реализовано

Задачи:

- реализовать `p010_bt2446c_to_sdr.wgsl`;
- перенести Method C stages в readable shader functions;
- реализовать P010 SDR BT.709 branch без BT.2446-C;
- использовать HDR uniforms;
- сохранить one-pass fullscreen path without intermediate 8-bit clamp;
- добавить source tests для P010 plane order и запрещённых SDR shortcuts.

Unit tests:

- shader source reads P010 Y from plane 0 and UV from plane 1;
- shader contains PQ and HLG transfer branches or typed mode;
- P010 SDR test path does not call BT.2446-C functions;
- shader does not call NV12 SDR helper;
- shader contains BT.2446-C-specific stages;
- CPU reference constants match shader constants.

Manual tests:

- run HDR sample and verify no validation errors;
- visual check: HDR no longer washed-out on SDR monitor;
- SDR sample still visually matches previous path.

### Сессия 6: config propagation and diagnostics UI

Статус: реализовано

Задачи:

- добавить `[render.hdr_to_sdr]` config;
- пробросить `HdrToSdrSettings` до renderer;
- показать active HDR path в telemetry/media panel;
- показать reference-default markers;
- не добавлять alternative tone mapping UI controls.

Unit tests:

- config defaults valid;
- invalid operator rejected;
- invalid nits values rejected;
- UI diagnostics receives active path without GPU handles;
- old config migrates or defaults safely.

Manual tests:

- запуск без config создаёт HDR defaults;
- telemetry shows `P010 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf`;
- native HDR output shown as unsupported;
- SDR controls identity still do not change image.

### Сессия 7: integration and manual VP9 HDR verification

Статус: реализовано

Задачи:

- включить production capability intersection for HDR-to-SDR;
- запустить реальный VP9 Profile 2 HDR sample через полный Phase 10 path;
- проверить fallback/reject behavior на missing capability cases;
- проверить performance logs на отсутствие CPU fallback.

Unit tests:

- HDR stream selected only when decode + P010 renderable + HDR-to-SDR pass;
- missing required P010 import feature rejects stream: baseline separate-layer layout needs `TEXTURE_FORMAT_16BIT_NORM`, compatibility composed layout needs `TEXTURE_FORMAT_P010`;
- missing strict HDR metadata rejects stream;
- renderer error becomes fatal media error, not fallback.

Manual tests:

- VP9 SDR sample playback;
- VP9 HDR PQ sample playback;
- VP9 HDR HLG sample playback; если файла нет локально, sample скачивается через `yt-dlp`, а не пропускается;
- resize/letterbox works;
- black bars remain black;
- no CPU P010 upload/readback logs;
- active color path visible in UI.

### Сессия 8: self-review, cleanup and docs

Статус: реализовано

Задачи:

- пройти все HDR fallback paths и убедиться, что ошибок не игнорируют молча;
- проверить, что SDR path не зависит от HDR shader;
- проверить, что `app-egui` не содержит color math;
- проверить, что shader не стал unreadable monolith;
- обновить docs, если реализация уточнила детали.

Verification:

- `cargo fmt`;
- `cargo check`;
- targeted tests по affected crates;
- manual SDR VP9/NV12 playback;
- manual HDR VP9/P010 playback;
- capability report review;
- diagnostics review.

Итоги self-review:

- decoder-side P010 zero-copy failures теперь доходят до `PlayerSession` через `DecodeThreadError`;
- shell-side missing texture views и rejected `WgpuRenderableFrame` теперь идут
  через typed `PlayerRenderError`/`PlayerWorkerEvent::RenderError` и становятся
  fatal media errors в player snapshot;
- P010 SDR BT.709 side metadata с non-HDR transfer больше не выбирает HDR branch автоматически;
- `app-egui` по-прежнему только мапит config/diagnostics и не содержит PQ/HLG/BT.2446-C math;
- `nv12_to_rgba.wgsl` остался SDR shader, HDR math остаётся в `p010_bt2446c_to_sdr.wgsl`.

## Acceptance checklist

- Phase 9 P010 zero-copy boundary используется без CPU fallback.
- Phase 10 использует Intel/i965 separate-layer P010 path как baseline и не регрессирует его.
- HDR stream выбирается только при passing capability intersection.
- `supports_hdr_to_sdr = true` только после рабочей BT.2446-C реализации.
- `supports_native_hdr_output = false`.
- PQ и HLG supported.
- Strict HDR core metadata required.
- Optional mastering/CLL/FALL defaults visible in diagnostics.
- `P010` не трактуется автоматически как HDR.
- SDR P010 и HDR P010 имеют разные color paths.
- Current SDR VP9/NV12 path не регрессировал.
- `nv12_to_rgba.wgsl` остался SDR shader.
- `p010_bt2446c_to_sdr.wgsl` реализует HDR path отдельно.
- BT.2446-C math покрыта CPU reference tests.
- Shader constants/layout/source covered by tests.
- UI показывает active HDR color path.
- HDR renderer fail-closed, без washed-out fallback.

## Future work после Phase 10

- AV1 backend как новый producer P010/HDR contract.
- H.265 backend как новый producer P010/HDR contract.
- LUT-based optimization или alternative quality modes, если будет отдельное решение.
- Native HDR output через platform-specific compositor/display work.
- P012/12-bit HDR path.
- 4:2:2/4:4:4 renderer paths, если появится реальная hardware/use-case необходимость.
