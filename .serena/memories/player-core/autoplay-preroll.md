# Player Core Autoplay Preroll

- `PlayerSession` owns autoplay preroll policy. `begin_autoplay_preroll()` enters `PlaybackState::Buffering`; `finish_autoplay_preroll_if_ready()` is the only normal transition to `Playing` after the audio/video gates are satisfied.
- Audio readiness stays owned by the audio runtime boundary. Video readiness must not read pipeline storage directly; use `PlaybackPipeline` boundary methods.
- Video-only autoplay is ready when there is no selected video track, or when the pipeline already has either a current present frame or at least one queued decoded/presentation frame. This matters for H.264/MP4 B-frame startup where the first decoded display frame may be tens of milliseconds after zero; while still in `Buffering`, the no-audio monotonic media clock is not allowed to advance, so requiring an already-presented frame can deadlock startup.
- Focused coverage lives in `crates/player-core/src/session/tests/playback.rs`: `video_only_autoplay_keeps_present_frame_gate` and `video_only_autoplay_accepts_queued_preroll_frame`.


## Audio-aware demux starvation recovery (2026-08-27)

Temporary installed-demux starvation with selected audio now proactively freezes output when usable runway cannot cover remaining retry wait plus the canonically sanitized audio low-water scheduler/callback margin, independently of queued video. Remaining wait uses a fresh post-demux decision timestamp. Any exact current pending retry, including chained TUA after a recovered packet, blocks resume until the next matching accepted source event clears the source-wait fence; then the existing audio+video preroll gate is authoritative. Buffering resume publishes `Playing`/`AudioPlaybackResumed` only after successful output play and clock anchoring; recoverable play failure remains Buffering and is retried by the ordinary preroll lifecycle. Full invariants and tests: `mem:player-core/audio-starvation-buffering-resume-2026-08-27`.
