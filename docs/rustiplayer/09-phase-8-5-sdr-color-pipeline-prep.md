# 09. Phase 8.5 SDR Color Pipeline Prep

## Цель

Phase 8.5 - небольшой подготовительный refactor перед Phase 9 VP9 completion и Phase 10 HDR-to-SDR.

Задача этапа - сохранить текущий рабочий SDR VP9/NV12 путь, но убрать hardcoded color assumptions из shader-а и renderer implementation. После этапа renderer должен получать явные metadata/settings и строить GPU uniforms для текущего SDR path.

Это не HDR-реализация. Phase 8.5 не добавляет P010 playback, HDR tone mapping, native HDR output или wide-gamut output.

## Принятые решения

### Swapchain transfer

Выбран вариант 1C.

Вводим typed режим:

```rust
enum SwapchainTransferMode {
    PreserveCurrentUnorm,
    SrgbRenderTarget,
    ExplicitShaderOetf,
}
```

Default для Phase 8.5 - `PreserveCurrentUnorm`.

Причина: текущий renderer предпочитает `Bgra8Unorm/Rgba8Unorm`. В `wgpu` `UnormSrgb` применяет автоматическую sRGB conversion при записи в render target, а `Unorm` требует, чтобы shader output уже был display-referred. Поэтому переход на `SrgbRenderTarget` может изменить SDR результат и не входит в Phase 8.5.

### Color metadata source

Выбран вариант 2C.

Color metadata собирается layered-моделью с origin/confidence:

```rust
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
```

Порядок источников:

1. service manifest даёт ранний hint;
2. container metadata уточняет track-level fields;
3. codec bitstream parser подтверждает colorimetry;
4. decoder/backend подтверждает фактический decoded format;
5. fallback default используется только при отсутствии надёжной metadata.

Текущий fallback: `NV12 8-bit BT.709 limited SDR`.

### Tone mapping и config

Выбран вариант 3A + 3C.

В `render-core` закладываются typed tone mapping modes как future contract, но Phase 8.5 не показывает HDR tone mapping как пользовательскую настройку.

В user config можно добавить только SDR/RGB adjustments:

```toml
[render.color_adjustment]
brightness = 0.0
contrast = 1.0
saturation = 1.0
exposure = 0.0
rgb_gain = [1.0, 1.0, 1.0]
rgb_offset = [0.0, 0.0, 0.0]
```

Identity defaults обязательны, чтобы SDR картинка не изменилась.

### BT.2020 SDR

Выбран вариант 4C сейчас + 4D позже.

Phase 8.5 сохраняет BT.2020 metadata и честно показывает fallback diagnostics:

```text
NV12 8-bit BT.2020 limited -> SDR BT.709 fallback preserve-current-unorm
```

Это не означает поддержку wide-gamut output. Настоящий BT.2020 -> BT.709 gamut mapping добавляется позже, когда появится отдельная color-management задача.

## Non-goals

- Не реализовывать HDR tone mapping.
- Не объявлять `supports_hdr_to_sdr = true`.
- Не добавлять P010 в `RenderCapabilities::wgpu_nv12`.
- Не делать CPU readback/copy decoded frames ради цвета.
- Не добавлять CPU YUV->RGB conversion path.
- Не превращать `nv12_to_rgba.wgsl` в универсальный HDR shader.
- Не менять намеренно visual SDR result.

## Архитектура

```text
codec-core
  ColorRange, MatrixCoefficients, ColorPrimaries, TransferFunction,
  HdrMetadata, VideoColorMetadata, ColorMetadataOrigin
        |
        v
video-core
  DecodedFrame { pixel_format, bit_depth, chroma, color, texture_handle }
        |
        v
render-core
  RenderableFrame, ColorPipelineSettings, ActiveColorPath,
  SwapchainTransferMode, ToneMappingMode
        |
        v
render-wgpu
  ColorPipelineUniforms, NV12 matrix/range mapping,
  nv12_to_rgba.wgsl uniforms
        |
        v
app-egui
  diagnostics only, no color math
```

## Типы

### `codec-core`

Добавить:

- `ColorRange { Limited, Full, Unknown }`;
- `MatrixCoefficients { Bt601, Bt709, Bt2020, Unknown }`;
- `ColorPrimaries { Bt709, Bt2020, Smpte170m, Bt470Bg, Unknown }`;
- `ColorMetadataOrigin { FallbackDefault, Manifest, Container, Bitstream, DecoderBackend }`;
- `ColorMetadataConfidence { Fallback, Hint, Confirmed }`;
- `VideoColorMetadata { range, matrix, primaries, transfer, hdr_metadata, origin, confidence }`.

Расширить:

- `TransferFunction` должен иметь `Unknown`;
- `HdrMetadata` остаётся canonical HDR metadata model.

Добавить helper:

```rust
impl VideoColorMetadata {
    pub const fn sdr_bt709_limited() -> Self;
}
```

### `video-core`

Заменить упрощённый `ColorSpace` на typed metadata или оставить compatibility alias только на время миграции.

`DecodedFrame` должен хранить:

- `pixel_format`;
- `bit_depth`;
- `chroma`;
- `color`;
- `texture_handle`;
- coded/render sizes;
- `pts`.

### `render-core`

Добавить:

- `ColorPipelineSettings`;
- `ColorAdjustment`;
- `SwapchainTransferMode`;
- `ToneMappingMode`;
- `ActiveColorPath`;
- `OutputColorSpace::SdrBt709`.

`ColorPipelineSettings` для Phase 8.5:

```rust
struct ColorPipelineSettings {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    exposure: f32,
    rgb_gain: [f32; 3],
    rgb_offset: [f32; 3],
    tone_mapping: ToneMappingMode,
    swapchain_transfer: SwapchainTransferMode,
}
```

Defaults:

- `brightness = 0.0`;
- `contrast = 1.0`;
- `saturation = 1.0`;
- `exposure = 0.0`;
- `rgb_gain = [1.0, 1.0, 1.0]`;
- `rgb_offset = [0.0, 0.0, 0.0]`;
- `tone_mapping = Off`;
- `swapchain_transfer = PreserveCurrentUnorm`.

### `render-wgpu`

Добавить internal module `color_pipeline`.

Он отвечает за:

- перевод `VideoColorMetadata + ColorPipelineSettings` в `ColorPipelineUniforms`;
- выбор YUV range normalization;
- выбор YUV->RGB matrix;
- формирование `ActiveColorPath`;
- validation unsupported path без silent fallback.

`Nv12VideoRenderer` остаётся отдельным renderer path для SDR NV12.

`ColorPipelineUniforms` должен быть явно выровнен под WGSL uniform layout и покрыт tests через `bytemuck`.

Phase 8.5 matrix policy:

- `BT.709` SDR поддерживается как основной текущий path;
- `BT.601` SDR можно добавить через отдельные coefficients в том же uniform contract;
- `BT.2020` metadata сохраняется, но active path помечается как SDR BT.709 fallback до настоящего gamut mapping;
- `Unknown` использует `sdr_bt709_limited()` fallback с diagnostic note;
- `PQ`/`HLG` не проходят как поддержанный HDR path, пока `RenderCapabilities` не объявляет HDR support.

Перед каждой implementation-сессией нужно снова свериться с Context7 по внешним crate-ам, которые реально трогаются в этой сессии.

## Новый SDR data flow

1. Decoder создаёт `DecodedFrame` с `NV12`, `8-bit`, `YUV420`, `BT.709 limited SDR`.
2. `WgpuRenderableFrame::from_decoded_nv12` переносит metadata в `RenderableFrame`.
3. `WgpuVideoRenderer::render_or_clear` передаёт frame metadata в `Nv12VideoRenderer`.
4. `Nv12VideoRenderer` считает letterbox uniforms и color pipeline uniforms.
5. `queue.write_buffer` обновляет uniform buffer.
6. Shader читает Y, UV и применяет range/matrix/settings из uniforms.
7. Output path остаётся `SDR BT.709 preserve-current-unorm`.

## Декомпозиция по сессиям

### Сессия 1: typed metadata foundation

Статус: реализовано

Задачи:

- добавить color metadata types в `codec-core`;
- обновить exports в `codec-core/src/lib.rs`;
- обновить `video-core::DecodedFrame`;
- обновить места создания test frames и decoder fallback frames;
- сохранить compatibility текущего VP9/NV12 path.

Unit tests:

- `VideoColorMetadata::sdr_bt709_limited()` возвращает `Limited`, `Bt709`, `Bt709`, `Bt709`, `FallbackDefault`, `Fallback`;
- `ColorRange`, `MatrixCoefficients`, `ColorPrimaries`, `TransferFunction` сериализуются в ожидаемые snake_case значения, если типы используют `serde`;
- `DecodedFrame` test helper создаёт explicit metadata без старого `ColorSpace`.

Manual tests:

- `cargo check`;
- запуск текущего VP9/NV12 sample, если доступен;
- проверить logs, что fallback metadata не выглядит как bitstream metadata.

### Сессия 2: render-core contract

Статус: реализовано

Задачи:

- заменить `RenderColorSpace` на typed render color metadata;
- добавить `ColorPipelineSettings`, `ColorAdjustment`, `SwapchainTransferMode`, `ToneMappingMode`, `ActiveColorPath`;
- обновить `RenderableFrame`;
- обновить `RenderCapabilities` summary без преждевременного HDR support.

Unit tests:

- `ColorPipelineSettings::default()` identity;
- `ActiveColorPath` для `NV12 BT.709 limited -> SDR BT.709 preserve-current-unorm`;
- `RenderCapabilities::wgpu_nv12()` не поддерживает P010/HDR;
- ten-bit/HDR requirement всё ещё rejected текущим renderer.

Manual tests:

- capability report не обещает HDR;
- UI capability summary не регрессировал.

### Сессия 3: render-wgpu uniforms и shader boundary

Статус: реализовано

Задачи:

- добавить `render-wgpu/src/color_pipeline.rs`;
- добавить `ColorPipelineUniforms`;
- перенести BT.709 limited coefficients из shader constants в Rust-side defaults/uniforms;
- обновить bind group layout и uniform buffer size;
- обновить `nv12_to_rgba.wgsl`, чтобы он читал range/matrix/settings из uniforms;
- сохранить NV12 UV order.

Unit tests:

- BT.709 limited metadata -> uniforms численно совпадает со старой формулой;
- full range metadata -> uniforms использует full-range normalization;
- BT.2020 SDR metadata формирует active color path с fallback marker и не объявляет wide-gamut output;
- unknown metadata использует `sdr_bt709_limited()` fallback с diagnostic marker;
- PQ/HLG metadata не становится поддержанным HDR path при текущих capabilities;
- shader source содержит чтение `uv.r` как U и `uv.g` как V;
- shader source больше не содержит hardcoded BT.709-only function name как единственный contract.

Manual tests:

- VP9/NV12 SDR playback визуально как до refactor;
- resize/letterbox работает;
- black bars остаются чёрными;
- zero-copy success logs не исчезли;
- нет CPU color conversion/readback в runtime path.

### Сессия 4: config и UI diagnostics

Статус: реализовано

Задачи:

- добавить `render.color_adjustment` config defaults;
- пробросить settings до renderer без color math в UI;
- показать active color path в telemetry/media panel;
- не добавлять HDR controls до Phase 10.

Unit tests:

- config defaults identity;
- invalid RGB arrays rejected validation;
- UI snapshot/diagnostics получает active color path без GPU-specific handles.

Manual tests:

- запуск без config создаёт identity defaults;
- существующий config мигрирует или получает defaults без падения;
- telemetry panel показывает `NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm`;
- изменение будущих SDR sliders при identity values не меняет картинку.

### Сессия 5: self-review и cleanup

Задачи:

- пройти все fallback paths и убедиться, что ошибки не игнорируются молча;
- проверить, что names не стали абстрактными вроде `data`, `temp`, `obj`;
- проверить, что `app-egui` не содержит color math;
- проверить, что shader не стал HDR-комбайном;
- обновить docs, если реализация уточнила детали.

Verification:

- `cargo fmt`;
- `cargo check`;
- targeted unit tests по affected crates;
- ручной SDR VP9/NV12 playback;
- capability rejection для P010/HDR.

## Acceptance checklist

- Текущий SDR VP9/NV12 путь работает.
- SDR visual result не менялся намеренно.
- `BT.709 limited` shader math совпадает со старым path или имеет объяснимую погрешность.
- `RenderCapabilities` не объявляет HDR/P010 support.
- Active color path виден в diagnostics.
- Metadata -> uniforms покрыт unit tests.
- NV12 UV order покрыт shader/source test.
- Zero-copy DMA-BUF import остаётся целевым path.
- CPU readback/copy decoded frames не добавлен ради color pipeline.
- `nv12_to_rgba.wgsl` остался NV12 SDR shader path.

## Как это готовит Phase 9 и Phase 10

После Phase 8.5 Phase 9 сможет закрыть VP9 metadata/frame contract, а Phase 10 сможет добавить P010 renderer как отдельный path:

- `DecodedFrame` уже подготовлен к bit depth, pixel format и typed color metadata;
- `RenderableFrame` уже имеет renderer-neutral color boundary;
- `ColorPipelineSettings` уже содержит tone mapping future contract;
- `ActiveColorPath` уже умеет объяснять output path;
- swapchain transfer behavior уже не скрыт в порядке выбора surface format;
- SDR path уже защищён tests и не должен ломаться при Phase 9 VP9 completion или Phase 10 P010/HDR renderer.




ФАЗА ПОЛНОСТЬЮ ЗАКРЫТА!
