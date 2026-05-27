# 04. Кодеки и Capabilities

## Production policy

Видео считается поддержанным только после полного intersection:

- codec requirement известен достаточно точно;
- hardware backend заявил matching decode format;
- backend подтвердил обязательный `DMA-BUF` export;
- renderer подтвердил input surface format и P010 storage layout, если нужен;
- HDR stream прошёл strict metadata policy и renderer поддерживает HDR-to-SDR.

Software video fallback и CPU transfer не входят в production policy.

## Текущая матрица

| Stream | Surface | Decode | Render | Status |
| --- | --- | --- | --- | --- |
| VP9 Profile 0, 8-bit, 4:2:0, SDR | NV12 | VA-API | WGPU SDR BT.709 | production |
| VP9 Profile 2, 10-bit, 4:2:0, PQ/HLG HDR | P010 | VA-API | WGPU BT.2446-C to SDR BT.709 | production when capabilities pass |
| VP9 12-bit | none | rejected | rejected | unsupported bit depth |
| VP9 4:2:2/4:4:4 | none | rejected | rejected | unsupported chroma |
| VP9 Profile 1/3 | none | rejected | rejected | unsupported current renderer/backend path |
| AV1/H.264/H.265/VP8 | future | future | future | not production |

## Ключевые типы

Canonical types live in `codec-core`:

- `VideoCodec`
- `VideoProfile`
- `BitDepth`
- `ChromaSubsampling`
- `VideoSurfaceFormat`
- `VideoMemoryContract`
- `ZeroCopyExportRequirement`
- `VideoColorMetadata`
- `ColorMetadataOrigin`
- `ColorMetadataConfidence`
- `VideoDecodeRequirement`
- `SupportedVideoDecodeFormat`

Renderer-facing aliases and capabilities live in `render-core`:

- `RenderCapabilities`
- `P010RenderReadiness`
- `P010StorageLayout`
- `HdrToSdrSettings`
- `HdrToneMappingOperator`
- `ActiveColorPath`

## Bitstream probing

`player-core` may request generic refinement, but codec-specific parsing belongs
to `codec-core` adapters. VP9 uses `vp9-parser`. Future AV1/H.264/H.265 adapters
should wrap parser code already trusted by decode/backend code where possible.

Probe outcomes:

- `Candidate`: valid refined requirement can enter capability selection.
- `Rejected`: valid header proves unsupported codec/profile/bit-depth/chroma.
- `Recoverable`: header is incomplete, non-keyframe, uncertain or parser failed
  without proving the stream unsupported.

`Recoverable` must not become `HardwareDecoderUnavailable`.

## Color metadata

Color metadata is layered:

1. service manifest hint;
2. container track metadata;
3. codec bitstream confirmation;
4. decoder/backend decoded output confirmation;
5. explicit fallback default.

`VideoColorMetadata::sdr_bt709_limited()` is a fallback default, not proof from
media. Diagnostics must keep `origin` and `confidence` visible.

HDR processing is required by PQ/HLG transfer in core color metadata or matching
HDR side metadata. MaxCLL/MaxFALL alone beside BT.709 SDR does not make a stream
HDR.

## HDR rules

Current HDR-to-SDR production baseline:

- input surface: P010;
- bit depth: 10-bit;
- chroma: 4:2:0;
- transfer: PQ or HLG;
- primaries: BT.2020;
- matrix: BT.2020;
- range: explicit limited or full;
- operator: BT.2446 Method C;
- output: SDR BT.709;
- native HDR output: false.

If P010 renderer is unavailable, capability selection rejects the stream before
pretending HDR can be shown as SDR.

## Backend reports

`capability-core::SystemCapabilities` combines:

- `BackendCapabilities` from decode providers;
- `RenderCapabilities` from renderer backend;
- schema version `CURRENT_CAPABILITY_SCHEMA_VERSION = 2`;
- typed rejection reasons from `VideoCapabilityRejection`.

`video-vaapi::VaapiCapabilityProvider` supplies the current hardware backend
report. `render-wgpu-video` builds render capabilities from WGPU device
features, including `TEXTURE_FORMAT_16BIT_NORM` and `TEXTURE_FORMAT_P010`
implications for P010 layouts; `render-wgpu-shell` exposes that report to
`app-egui` during system capability probing.
