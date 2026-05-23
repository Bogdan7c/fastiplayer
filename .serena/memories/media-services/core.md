# Media Services

- `media-core` owns neutral `Packet`, `TrackInfo`, `MediaTime`, timeline/snapshot contracts. Packet payload uses `bytes::Bytes`; cloning shares payload ownership.
- `codec-core` owns canonical codec/profile/color/surface/memory requirement types and codec adapters. Codec-specific parsing belongs here; VP9 parser is wrapped via `vp9-parser`.
- `capability-core::SystemCapabilities::select_best_video_stream()` is the selection gate. Use typed `VideoCapabilityRejection` when rejection affects behavior/diagnostics.
- `symphonia-demux` is the concrete upstream Symphonia demux adapter. `webm-demux` is compatibility re-export for old demux crate path during transition.
- Demux seek returns actual container/decode-safe or approximate position; `player-core` owns final pre-roll/drop/commit semantics.
- `source-core` owns byte access only: local file, HTTP Range, cached byte source, RAM byte-range cache, streaming reader, runtime source config, cancellation/diagnostics. It must not know YouTube, containers, codecs, renderer, UI, or player state.
- `HttpRangeSource` uses caller-provided headers, probes seekability, and has limited retry policy. Propagate non-seekable state; do not invent byte-offset seek hints above source/demux boundary.
- `service-youtube` owns YouTube/yt-dlp details, direct stream URL/header resolution, refreshable HTTP Range source for VOD, and live/non-seekable fallback. It returns ready streaming media/demuxer to shell.
- YouTube cookies/session data must not be stored in TOML config. Future persistent credentials need a separate OS credential/session boundary.
- Future services should return normalized candidates with source descriptors, headers, codec hints, then let `capability-core` select before demux/player open.