# 07. Services и Network

## Source boundary

`source-core` owns byte access:

- `ByteSource`;
- `LocalFileSource`;
- `HttpRangeSource`;
- `CachedByteSource`;
- `RamByteRangeCache`;
- `StreamingByteReader`;
- `SourceRuntimeConfig`;
- cancellation and source diagnostics.

It does not know YouTube, containers, codecs, renderer or UI.

## HTTP Range

`HttpRangeSource` opens direct media URLs with headers supplied by a caller. It
performs a Range seekability probe and reads into caller-provided buffers. Retry
policy is limited: one retry for retryable range/body failures.

`Seekability::NotSeekable` is propagated into demux timeline state. The player
must not invent byte-offset seek hints above the demuxer/source boundary.

## Cache

Current cache is RAM byte-range cache. Public knobs:

- `network.memory_cache_mb`;
- `network.read_ahead_mb`;
- `network.connect_timeout_ms`;
- `network.read_timeout_ms`.

Durable byte cache and durable metadata are future work. They must not add IO to
playback, seek or scrub hot paths without a clear boundary.

## Demux boundary

`webm-demux` owns WebM/Matroska demuxing through Symphonia and Matroska pre-scan.
It exposes `Demuxer`, `DemuxSeekRequest`, `DemuxSeekResult` and `DemuxSeekability`.

Rules:

- packet payload uses `bytes::Bytes`;
- unknown video codec stays `unknown_video`, not assumed VP9;
- corrupted packets can be skipped only up to configured fail-safe limit;
- seek returns actual container position, while precise commit happens in `player-core`.

## YouTube boundary

`service-youtube` is a service adapter, not player logic.

Current implementation:

- uses `yt-dlp`;
- resolves direct video/audio stream URLs and headers;
- wraps expiring direct URLs with refreshable HTTP Range source for VOD when possible;
- falls back to unseekable streaming source for live or non-seekable media;
- returns `YoutubeStreamingMedia` with a ready demuxer and description.

`app-egui` запускает подготовку CLI YouTube URL на background thread, поэтому
создание окна и UI не блокируются.

## Future service model

Future service adapters should return normalized candidates instead of selecting
only one playable stream internally:

```text
service candidate
  -> source descriptors and headers
  -> media/core codec metadata hints
  -> capability-core selection
  -> demux/player open
```

Cookies/session data нельзя хранить в TOML. Если persistent credentials станут
нужны, нужен отдельный OS credential/session boundary.
