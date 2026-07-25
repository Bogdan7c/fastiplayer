# S36F3A — exact packet presentation-window transport (2026-07-25)

## Neutral value owner
- `media-core` owns `ExactPresentationWindow` over exact `TrackTimestamp` boundaries and `PacketPresentationWindow::{Unbounded, Bounded}`. Construction rejects track/timebase mismatch, negative start, empty/reversed intervals; no Default, Option, float, Duration rescale, epsilon or tolerance exists.
- `Packet` stores the window privately. Existing constructors were replaced by explicit `new_unbounded` / `new_with_keyframe_unbounded`; all local/progressive/HLS/DASH/FLV/MPEG-TS/Symphonia producers and test fakes migrated explicitly without behavior changes.
- Bounded assignment is fallible and requires matching packet track, matching raw timebase and an actual raw `track_pts`; packets without exact presentation-clock evidence cannot be bounded. `with_track_id` atomically remaps packet timing and both bounded boundaries.
- One narrow documented Clippy `large_enum_variant` allow remains on the internal progressive worker message to preserve inline packet ownership and avoid a new per-packet heap allocation.

## Provenance-aware ordered ISO demux
- `demux-api` adds a parallel `PresentationWindowOrderedSegment` / source/read-outcome contract. Existing `OrderedSegment`, `DemuxInput` and finite HLS/DASH paths were not extended or changed.
- `symphonia-demux::PresentationWindowOrderedIsoMp4Demuxer` requires one init and strictly increasing media sequence, rejects second init/media-before-init/new-timeline discontinuity, remains not seekable, preserves temporary readiness and publishes terminal EOS only after draining the active fragment.
- Every media fragment is opened through the existing ISO registry as an isolated shared-bytes reader of `init + exactly one media fragment`. Packets are tagged with that fragment's declared window before publication. Provenance is never inferred from PTS or current-reader state, which is essential because approved Smooth audio coded ranges overlap across adjacent fragments.
- Track snapshot must remain single-track and stable across isolated opens; codec/private data/timebase/layout drift and inner `TracksChanged` fail typed. Cancellation fences exist before source pull, fragment open and packet publication. The reader does no full concatenation and no per-read `Bytes` clone/refcount churn.

## Player transport-only boundary
- `player-core::PendingAudioPacket` is owned by a dedicated internal module with private fields and intent getters. It carries the exact window through demux routing, bounded pending queue, throttled requeue, generation checks and decoder dispatch.
- Immediately after `DecodedAudioPacket::Pcm`, internal `DecodedPcmPacketBoundary` receives borrowed samples plus the packet window before existing global/CUE playback trimming. In F3A it returns samples unchanged; bounded and unbounded byte/sample/accounting parity is explicitly tested.
- Decoder/audio public traits, `EncodedAudioPacket`, output traits, PreparedMedia and existing global/CUE trim semantics are unchanged. Stale generation drops packet+window together; seek/reset/media replacement retain existing pending-queue clearing.

## S36F3B completion
- `player-core` now consumes bounded windows at `DecodedPcmPacketBoundary` with checked signed rational ceil math, intersects packet and legacy global/CUE ranges in the original decoded frame domain, and passes only retained frames to tempo/history/output/accounting. Fully outside packets decode but create no output side effect. Full clipping contract: `mem:player-core/exact-pcm-presentation-window-s36f3b-2026-07-25`.

## Verification and completed downstream use
- Accepted F3A checks include media-core 53 tests, demux-api 40 tests, symphonia-demux 171 plus 9 focused adapter tests, player-core 599 tests, strict Clippy, impacted HLS/DASH/FLV/MPEG-TS/VA-API checks, full workspace check, guardrails, fmt, diff and Serena diagnostics.
- F3B exact PCM clipping is complete (`mem:player-core/exact-pcm-presentation-window-s36f3b-2026-07-25`), and P4–P6 now use the F3A adapter in the production Smooth runtime (`mem:media-services/smooth-vod-runtime-s36p4-p6-2026-07-25`).