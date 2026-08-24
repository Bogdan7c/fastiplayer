# Hardware AV1 Main/Profile 0 (2026-08-24)

## Production scope

- Native VA-API playback supports only AV1 Main / VA Profile 0 with YUV420: 8-bit -> NV12 DMA-BUF and 10-bit -> P010 DMA-BUF. AV1 High, Professional, 12-bit, YUV422, YUV444, mismatched surface/bit depth, software-host contracts and codec-specific packetization metadata remain typed rejects.
- Existing `codec-core` AV1 `av1C`/keyframe parsing and existing NV12/P010 renderer materializers remain their owners. New decode state is private to `video-vaapi`; no public `player-core`, `video-core`, `video-backend-api` or renderer API was widened.
- App selection uses the existing requirement/capability boundary: `auto` and `hardware` select `VaapiDmaBufWgpu` when a matching AV1 Main output is playable; `auto` falls back to FFmpeg only when no matching native output exists; strict `hardware` never silently falls back.

## Codec-owned input invariant

- `cros-codecs::StatelessDecoder<Av1, VaapiBackend<InternalVaapiFrame>>` consumes exactly one OBU per `decode()`, while demux packets contain a whole temporal unit.
- `codec_adapter/av1.rs` therefore owns a copied pending temporal unit, exact consumed offset and retry identity `(timestamp_us, full packet bytes)`. It ACKs the source packet only after all OBU bytes were consumed.
- `CheckEvents` and `NotEnoughOutputBuffers` preserve the pending bytes/offset. Different retry identity, zero consume, over-consume and terminal parse/decoder/backend errors fail closed and recycle input. Flush discards old-generation partial input before cros flush; EOF drain rejects a partial temporal unit.
- Do not replace this with the VP9 one-call adapter or with shared generic AU state: the one-OBU contract and lifecycle are AV1-owned.

## Probe and acceptance

- `VaapiCodecAdapterFactory` is the single implemented-format whitelist. Capability probing publishes AV1 only for exact `VAProfileAV1Profile0`; Profile 1/High remains filtered even if the driver reports it.
- Runtime hardware suites require headless `vainfo --display drm --device <node>` and exact `VAProfileAV1Profile0 : VAEntrypointVLD`. SDR requires configured AV1 adapter + NV12 registration; HDR requires AV1 adapter + P010; both require exact `video frame submitted to renderer` and forbid FFmpeg fallback/reselection/fatal markers.
- `scripts/playback-smoke.sh --mode full` additionally keeps a real FFmpeg AV1 SDR renderer regression. Its installed-runtime probe must select only `--test ffmpeg_runtime_probe -- --ignored --exact installed_ffmpeg_runtime_probe_reports_available_runtime`; a crate-wide `-- --ignored` incorrectly runs unrelated fixture/WGPU regressions.

## Verified on 2026-08-24

- Host: AMD Radeon 780M, Mesa 26.2.1, VA-API 1.24 with Profile0 VLD.
- Real `hardware-only` and `full` runs passed on repository VP9 Profile0, AV1 Main 8-bit SDR, AV1 Main 10-bit HDR, H.264 and software regression fixtures. Both AV1 hardware scenarios proved VAAPI adapter, exact NV12/P010 DMA-BUF and renderer submit.
- Hermetic checks: `video-vaapi` 160/160; app selector 19/19; renderer accounting 1/1; playback shell self-test; strict workspace Clippy; fmt, diff and refactor guardrails. AV1 adapter lifecycle tests live with `codec_adapter/av1.rs`; factory policy tests with `codec_adapter/factory.rs`; oversized-module-safe probe tests in `probe/tests/av1.rs`; selector tests in `video_pipeline_selector/tests/av1.rs`.
