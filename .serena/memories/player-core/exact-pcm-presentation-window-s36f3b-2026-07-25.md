# S36F3B — exact decoded PCM presentation-window clipping (2026-07-25)

## Ownership and placement
- `player-core` owns exact packet-window PCM clipping at internal `DecodedPcmPacketBoundary`, immediately after successful decode/generation/track fences and before tempo, passthrough history and audio output.
- Decoder/audio public traits, `EncodedAudioPacket`, `media-core` window API, PreparedMedia and output contracts are unchanged. Unbounded packets require no new timing/layout evidence and retain the prior global/CUE behavior.

## Exact bounded arithmetic
- A bounded packet validates exact raw audio timing/timebase and track identity against the window, nonzero decoded sample rate/channels, complete interleaved frames and checked sample indexing. Failure is a fatal runtime error before output mutation; bounded never falls back to unbounded.
- For each half-open boundary, frame index is `ceil((boundary_units - packet_pts_units) * time_base.numer * sample_rate / time_base.denom)`, computed with checked signed `i128`, factor reduction and a project-owned signed-ceil helper. Results clamp only to the decoded frame count; there is no float, Duration/nanosecond conversion, epsilon, tolerance or saturation.
- Both the packet-window range and the existing global/CUE range are computed in the original decoded frame domain, intersected, then applied as one borrowed interleaved slice. Sequential trim with stale packet PTS is forbidden. The old global/CUE Duration/floor calculation is preserved literally.

## Runtime and accounting semantics
- A packet fully outside its bounded window is still decoded so codec state advances, then succeeds with no output creation/write, passthrough history, tempo input, submitted-frame accounting, clock compensation, silence insertion or EOF drain tail.
- For retained packets, only allowed frames reach passthrough history, tempo, direct output and all frame reports/accounting. A nonempty packet range whose global/CUE intersection is empty preserves the pre-existing output lifecycle but performs no write.
- Removed leading/tail frames are outside presentation time; player does not retime, reanchor or fill gaps. Provider composition must supply continuous aligned manifest windows.

## Exact Smooth proof and verification
- At 10,000,000 ticks/s and 48 kHz, the first approved AAC fragment keeps `ceil(39_680_000 * 48_000 / 10_000_000) = 190_464` of 192,512 decoded frames, removing exactly 2,048.
- The second keeps `ceil(39_893_333 * 48_000 / 10_000_000) = 191_488` of 191,488 frames; the exact one-tick overhang removes zero because no PCM frame starts outside the half-open manifest window.
- Tests cover exact/fractional boundaries, negative packet PTS, fully before/after, stereo/multichannel alignment, rate/timescale differences, invalid metadata/layout/overflow, original-domain range intersection, partial direct output, full drop without output mutation, retained-only history/accounting, clipped tempo input, fatal pre-output validation, stale generation and Unbounded parity.
- Accepted verification: player-core 611 tests, strict all-target Clippy, media-core/demux-api/symphonia checks, full workspace all-features check, guardrails, fmt, diff and Serena diagnostics.

## Remaining S36 work
- This capability permits F2 `PendingExactAudioClipping` to be mapped into the window-aware audio source, but does not itself register or admit Smooth playback. Production composition still owns manifest/fragment fetching, selected A/V construction, reconstruction, bounded audio source mapping, exact alignment, VOD seek rebuild/generation receipts, cancellation/readiness/EOS, provider registration and end-to-end fixtures. See `mem:media-services/presentation-window-transport-s36f3a-2026-07-25` and `mem:media-services/smooth-streaming-fmp4-mapper-s36f2-2026-07-25`.