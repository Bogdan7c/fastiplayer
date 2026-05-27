# Audio Core / Concrete Audio

- Concrete `crates/audio` owns CPAL output, concrete `AudioClock`, Symphonia/Opus decoder integration, and adapters to neutral `audio-core` traits.
- `AudioClock::samples_played` is buffer/accounting state: it counts interleaved samples pulled by CPAL callback and may include a callback buffer that is not audible yet.
- `AudioClock::now()` / `PlayerAudioClock::now()` must return stable media time based on CPAL playback anchors plus `last_stable_played_samples`; seqlock write/conflict/no-anchor fallback must never return raw `samples_played` from the fresh callback end.
- Player scheduling must not compensate for audio future jumps in `player-core/tick`; the clock invariant belongs in `crates/audio`.
- Silence/underrun callbacks may extend output duration for timestamp chaining, but media clock must clamp to real filled audio samples and not advance into silence.
- Focused clock tests live in `crates/audio/src/clock.rs` and cover unfinished anchor write, generation conflict fallback, normal interpolation, reset, legacy `record_played`, and silence tail behavior.