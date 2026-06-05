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
| H.264 ConstrainedBaseline/Main/High, 8-bit, 4:2:0, SDR | NV12 | VA-API | WGPU SDR BT.709 | production when capabilities pass |
| H.265 Main, 8-bit, 4:2:0, SDR | NV12 | VA-API | WGPU SDR BT.709 | production when capabilities pass |
| H.265 Main10, 10-bit, 4:2:0, PQ/HLG HDR | P010 | VA-API | WGPU BT.2446-C to SDR BT.709 | production when capabilities pass |
| AV1/VP8 and future H.265 profiles | future | future | future | not production |

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
to `codec-core` adapters. VP9 uses `vp9-parser`; H.264 and H.265 use
codec-core packet/config parsers that match the VA-API adapter contract. Future
adapters should wrap parser code already trusted by decode/backend code where
possible.

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

Bitstream refinement must not erase container color metadata. H.264/H.265
config or packet candidates are authoritative for profile, bit depth, chroma and
surface format when they know those fields, but MP4/Matroska `colr`/HDR metadata
must remain in `VideoDecodeRequirement.color` when the bitstream parser has no
equivalent color proof. Otherwise `DecodePacket.resolved_color` becomes `None`
and the renderer falls back to SDR BT.709 instead of the HDR-to-SDR path.

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

## H.265 manual validation notes

H.265 is advertised only through the normal production intersection. VA-API
must report HEVC Main/Main10 decode support, DMA-BUF export must stay available,
and renderer support must accept NV12 or P010 with the active color policy.

Validated local fixture classes:

- `hvc1` canonical `hvcC`: Main 8-bit MP4 and Main10 HDR MP4.
- `hev1` / in-band parameter sets: weak sample-entry handling remains covered
  by the `hev1` MP4 and raw Annex B fixture when available locally.
- Seek/flush B-frame/DPB smoke: generated 20 s 4K60 samples include B-frames;
  manual checks should verify that the first post-seek frame is not stale.
- Zero-copy diagnostics: Main path must log NV12 DMA-BUF; Main10 path must log
  P010 DMA-BUF and BT.2020/PQ HDR-to-SDR renderer dispatch.

Known fixture gaps do not block local H.265 Main/Main10 testing:

- Android HEVC originals are still missing.
- iOS HEVC originals should be kept untranscoded; current local coverage uses an
  iPhone Main10 `hvc1` MOV base-layer sample when present.
- Incomplete `hvcC` remains a weak-file validation gap unless a local asset is
  explicitly added for it.
