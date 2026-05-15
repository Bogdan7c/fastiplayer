# 09. Контракты и Internal API

## Player boundary

Публичный внутренний API `player-core`:

- commands: `PlayerCommand`, `PlayerCommandSender`;
- state: `PlayerSnapshot`, `PlaybackState`;
- events: `PlayerEvent`, `PlayerWorkerEvent`;
- runtime: `PlayerWorker`, `PlayerWorkerConfig`;
- render lease: `PresentFrameLease`, `PlayerPresentFrame`, `PresentFrameTextureViews`;
- render errors: `PlayerRenderError`, `PlayerRenderErrorKind`;
- seek: `SeekRequest`, `SeekTarget`, `SeekMode`, `ScrubCommitPolicy`;
- backend init boundary: `VideoBackendFactory`, `WgpuVideoBackendFactory`.

Контракт:

- UI отправляет команды и читает snapshots.
- Worker владеет `PlayerSession`.
- Render thread получает frame leases, а не ссылки на pipeline.
- Частые scrub updates схлопываются по latest-wins семантике.

## Media/demux contract

`media-core::Packet` является типом передачи packets между demuxer и player.
Payload хранится как `Bytes`, поэтому clone означает shared ownership, а не копию
payload.

`webm-demux::Demuxer` returns packets and supports timeline seek through
`DemuxSeekRequest`. Demuxer seek gives a decode-safe or approximate container
position; `player-core` owns final pre-roll/drop/commit.

## Codec contract

`VideoDecodeRequirement` является единственным объектом stream requirement,
который попадает в capability selection. Он объединяет codec/profile/bit-depth/
chroma/resolution, surface format, memory contract, color pipeline requirement и
timing contract.

Codec adapters могут уточнять requirements. Они не должны напрямую открывать
backend, renderer, UI или source resources.

## Capability contract

`SystemCapabilities::select_best_video_stream()` является selection gate.

Selection должна учитывать:

- supported decode format;
- mandatory export path from `VideoMemoryContract`;
- renderer format support;
- P010 readiness and storage layout;
- strict HDR metadata;
- renderer HDR-to-SDR settings.

Ошибки должны использовать `VideoCapabilityRejection`, а не generic strings,
если причина влияет на user-facing поведение.

## Video decode contract

`video-vaapi::VideoDecodeThread` owns backend threading and queues. It accepts
`DecodePacket` and publishes `video_core::DecodedFrame`.

Decoded frame contract:

- `format`: `Nv12` or `P010` for production paths;
- `memory_path`: `DmaBufZeroCopy`;
- `texture_handle`: opaque handle, not a CPU image;
- `color`: resolved `VideoColorMetadata`;
- diagnostics travel with the frame.

`VideoTextureViewProvider` является render-side bridge для WGPU texture views.

## Render contract

`render-wgpu::WgpuRenderableFrame` validates decoded frame metadata before render.

Allowed constructors:

- `from_decoded_nv12`;
- `from_decoded_p010`.

Оба конструктора отвергают non-zero-copy memory paths. Metadata/plane mismatch
является render boundary error.

`RenderDiagnostics` renderer-neutral: UI может показывать его без GPU handles.

## Config contract

`AppConfig::validate()` обязателен после deserialization. Defaults принадлежат
коду и описаны в [05. Config and Runtime Data](05-config-and-storage.md).

Unknown fields являются ошибками. Silent fallback для invalid config запрещён,
если validation явно не документирует compatibility mapping.

## Service/source contract

`service-youtube` may know YouTube and `yt-dlp`. It may not know renderer,
playback queues or UI layout.

`source-core::ByteSource` exposes read/seek/position/seekability/validators.
Service-specific headers are data, not hardcoded source policy.
