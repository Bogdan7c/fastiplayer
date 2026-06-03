# Player Core Autoplay Preroll

- `PlayerSession` owns autoplay preroll policy. `begin_autoplay_preroll()` enters `PlaybackState::Buffering`; `finish_autoplay_preroll_if_ready()` is the only normal transition to `Playing` after the audio/video gates are satisfied.
- Audio readiness stays owned by the audio runtime boundary. Video readiness must not read pipeline storage directly; use `PlaybackPipeline` boundary methods.
- Video-only autoplay is ready when there is no selected video track, or when the pipeline already has either a current present frame or at least one queued decoded/presentation frame. This matters for H.264/MP4 B-frame startup where the first decoded display frame may be tens of milliseconds after zero; while still in `Buffering`, the no-audio monotonic media clock is not allowed to advance, so requiring an already-presented frame can deadlock startup.
- Focused coverage lives in `crates/player-core/src/session/tests/playback.rs`: `video_only_autoplay_keeps_present_frame_gate` and `video_only_autoplay_accepts_queued_preroll_frame`.
