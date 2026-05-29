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
- `network.prefetch_chunk_mb`;
- `network.connect_timeout_ms`;
- `network.read_timeout_ms`.

Durable byte cache and durable metadata are future work. They must not add IO to
playback, seek or scrub hot paths without a clear boundary.

## RAM read-ahead prefetch

`media-prefetch` adds a RAM-only read-ahead layer for seekable Range-backed
sources. The layer wraps a `source-core::ByteSource`, moves the inner source into
one background worker thread and keeps a bounded sliding window in memory.

Policy comes from `[network]` in user TOML, but the mapping lives in
`service-youtube`: `network.prefetch_chunk_mb` becomes one worker read chunk, and
`network.read_ahead_mb` becomes the target RAM window. Defaults are 8 MiB chunk
and 256 MiB window. The window is bounded per wrapped source and does not grow
with the media duration.

Foreground demux reads are served from RAM. Network IO is owned by the worker,
so the playback foreground path does not synchronously perform HTTP reads after
the prefetch window is primed. Seek outside the current window resets the RAM
window and continues from the new absolute byte offset.

Live streams and direct URLs that fail the Range seekability probe still use the
separate playback-only streaming fallback (`StreamingByteReader` +
`spawn_http_fetcher`). That path remains non-seekable and is not wrapped in
`media-prefetch`.

## Demux boundary

`symphonia-demux` owns concrete demuxing through Symphonia and Matroska pre-scan.
It exposes `Demuxer`, `DemuxSeekRequest`, `DemuxSeekResult` and
`DemuxSeekability` through the neutral `media-core` contract. `webm-demux`
remains only a compatibility re-export for the old crate path.

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
- wraps seekable VOD Range sources in RAM read-ahead prefetch;
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
