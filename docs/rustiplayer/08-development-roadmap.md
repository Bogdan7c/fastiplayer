# 08. Roadmap разработки

## Текущий baseline

Implemented baseline:

- worker-owned playback runtime;
- neutral media/time/timeline types;
- TOML config schema v2;
- capability and renderer intersection;
- VA-API VP9 NV12/P010 decode path;
- WGPU NV12 and P010 HDR-to-SDR render paths;
- zero-copy render lease boundary;
- YouTube VOD startup through background shell job and `yt-dlp`;
- source-core HTTP Range and RAM cache boundary;
- desktop integration adapter boundary.

## Ближайшая работа

1. Clean remaining module boundaries documented in
   [10. Module Boundaries and Debt](10-module-boundaries-and-debt.md).
2. Make service stream selection capability-aware instead of YouTube defaulting to
   an SDR selector.
3. Add AV1 only after codec adapter, capability report, backend validation and
   renderer intersection are ready.
4. Add MP4/fMP4 only through a demux/source boundary that preserves current
   `media-core` contracts.
5. Keep smooth playback verification tied to diagnostics stages, not blanket
   buffer increases.

## Отложенная работа

- Native HDR output.
- OpenGL ES renderer.
- Windows DX12 backend.
- macOS backend.
- Durable history/bookmarks/cache metadata.
- Rust-native YouTube extractor replacement.
- DRM/protected content.

## Правило для новой работы

Every new feature must answer:

- Which crate owns the contract?
- Which crate owns IO/backend/platform details?
- Which typed error or reject explains unsupported state?
- Does it preserve hardware zero-copy video invariant?
- Which test or manual verification protects the boundary?
