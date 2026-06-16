# 04. Кодеки и Capabilities

## Production policy

Видео считается поддержанным только после полного intersection:

- codec requirement известен достаточно точно;
- decode backend заявил matching `SupportedVideoOutput`;
- backend-declared transfer path пересёкся с renderer-declared `VideoFrameContract`;
- renderer подтвердил input pixel layout и hardware handle layout, если нужен;
- HDR stream прошёл strict metadata policy и renderer поддерживает HDR-to-SDR.

VA-API hardware остаётся preferred production path. FFmpeg software decode
становится playable только когда runtime probe успешен, provider объявил raw
software output, а renderer подтвердил точный `SoftwareHostUpload` contract.
CPU RGB conversion, swscale playback conversion, CPU readback и FFmpeg hardware
decode не входят в policy.

## Текущая матрица

| Stream | Frame contract | Decode | Render | Status |
| --- | --- | --- | --- | --- |
| VP9 Profile 0, 8-bit, 4:2:0, SDR | NV12 + DMA-BUF | VA-API | WGPU SDR BT.709 | production |
| VP9 Profile 2, 10-bit, 4:2:0, PQ/HLG HDR | P010 + DMA-BUF | VA-API | WGPU BT.2446-C to SDR BT.709 | production when capabilities pass |
| VP9 12-bit | none | rejected | rejected | unsupported bit depth |
| VP9 4:2:2/4:4:4 | none | rejected | rejected | unsupported chroma |
| VP9 Profile 1/3 | none | rejected | rejected | unsupported current renderer/backend path |
| H.264 ConstrainedBaseline/Main/High, 8-bit, 4:2:0, SDR | NV12 + DMA-BUF | VA-API | WGPU SDR BT.709 | production when capabilities pass |
| H.265 Main, 8-bit, 4:2:0, SDR | NV12 + DMA-BUF | VA-API | WGPU SDR BT.709 | production when capabilities pass |
| H.265 Main10, 10-bit, 4:2:0, PQ/HLG HDR | P010 + DMA-BUF | VA-API | WGPU BT.2446-C to SDR BT.709 | production when capabilities pass |
| H.264 ConstrainedBaseline/Main/High, 8-bit, 4:2:0 | YUV420 HostPlanar 8-bit | FFmpeg software | WGPU HostPlanar upload | playable when FFmpeg/runtime/renderer pass |
| VP8 Version0To3, 8-bit, 4:2:0 | YUV420 HostPlanar 8-bit | FFmpeg software | WGPU HostPlanar upload | playable when FFmpeg/runtime/renderer pass |
| H.265 Main/Main10/Main12/Main422_10/Main422_12/Main444/Main444_10 | matching HostPlanar YUV | FFmpeg software | WGPU HostPlanar upload | playable when FFmpeg/runtime/renderer pass |
| VP9 Profile 0/1/2/3 legal v1 layouts | matching HostPlanar YUV | FFmpeg software | WGPU HostPlanar upload | playable when FFmpeg/runtime/renderer pass |
| AV1 Main 8/10-bit 4:2:0, AV1 High 8/10-bit 4:4:4 | matching HostPlanar YUV | FFmpeg software | WGPU HostPlanar upload | playable when FFmpeg/runtime/renderer pass |
| 4:4:4 12-bit software layouts | none | rejected | rejected | outside v1 HostPlanar matrix |

## Ключевые типы

Canonical types live in `codec-core`:

- `VideoCodec`
- `VideoProfile`
- `BitDepth`
- `ChromaSubsampling`
- `VideoColorMetadata`
- `ColorMetadataOrigin`
- `ColorMetadataConfidence`
- `VideoDecodeRequirement`
- `SupportedVideoDecodeFormat`

Frame output contract types live in `video-frame-contract` and are referenced
by decoded frames, renderer capabilities and capability reports:

- `VideoFramePixelLayout`
- `VideoFrameContract`
- `VideoFrameTransferPath`
- `HardwareFrameHandle`
- `DmaBufImageLayout`

Capability selection binds codec and output contracts through
`capability-core::SupportedVideoOutput`.

Renderer-facing aliases and capabilities live in `render-core`:

- `RenderCapabilities`
- `P010RenderReadiness`
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
config or packet candidates are authoritative for profile, bit depth and chroma
when they know those fields, but they do not select the concrete transfer path.
The output path comes from `SupportedVideoOutput.frame_contract`, selected after
backend and renderer capability intersection. MP4/Matroska `colr`/HDR metadata
must remain in `VideoDecodeRequirement.color` when the bitstream parser has no
equivalent color proof. Otherwise `DecodePacket.resolved_color` becomes `None`
and the renderer falls back to SDR BT.709 instead of the HDR-to-SDR path.

## HDR rules

Current HDR-to-SDR production baseline:

- input frame contract: P010 + DMA-BUF;
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
- schema version `CURRENT_CAPABILITY_SCHEMA_VERSION = 5`;
- typed rejection reasons from `VideoCapabilityRejection`.

`video-vaapi::VaapiCapabilityProvider` supplies the current hardware backend
report. `video-ffmpeg::FfmpegSoftwareCapabilityProvider` supplies the optional
software backend report only after FFmpeg runtime probing. Probe failures are
typed in diagnostics as `no-build`, `missing-runtime-libs`, `too-old` or
`probe-failed`.

Each `BackendCapabilities` entry keeps raw backend outputs in
`raw_supported_outputs`; each `SupportedVideoOutput` binds backend id,
codec-level decode format and provider-declared `VideoFrameContract` in one
record. `SystemCapabilities::playable_video_outputs` stores only the
system-level intersection with renderer support, so diagnostics can explain
backend-capable-but-renderer-incompatible outputs without inventing a false
Cartesian product between codec formats and transfer paths. Report text prints
the backend id, pixel layout and transfer path for each output.

`render-wgpu-video` builds render capabilities from WGPU device features,
including `TEXTURE_FORMAT_16BIT_NORM` and `TEXTURE_FORMAT_P010` implications
for P010 DMA-BUF layouts; `render-wgpu-shell` exposes that report to `app-egui`
during system capability probing. Renderer support is expressed as full
`VideoFrameContract` entries, so pixel layout, transfer path, and hardware
handle layout are checked as one contract instead of separate format/layout
lists.

Current production `SupportedVideoOutput` records can be:

- VA-API hardware outputs with `HardwareZeroCopy { DmaBuf { image_layout } }`;
- FFmpeg software outputs with explicit HostPlanar YUV pixel layout and
  `SoftwareHostUpload`.

`app-egui` selects concrete plans from already-playable outputs:
`auto` prefers VA-API DMA-BUF and then FFmpeg HostPlanar, `hardware` never falls
back to software, and `software` never starts VA-API.

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
