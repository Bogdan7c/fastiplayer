# S42 playback dedicated unit-test layout (2026-08-27)

- Behavior-neutral relocation only: production API/runtime behavior, FFI/time-base semantics, Symphonia track-mapping semantics, test names and assertions are unchanged.
- `crates/video-ffmpeg/src/ffi/codec_context.rs` now declares private `#[cfg(test)] mod tests;`; its 10 FFmpeg-feature unit tests (6 without the feature) live in `crates/video-ffmpeg/src/ffi/codec_context/tests.rs`.
- `crates/symphonia-demux/src/track_mapper.rs` now declares private `#[cfg(test)] mod tests;`; its 27 unit tests live in `crates/symphonia-demux/src/track_mapper/tests.rs`.
- Both out-of-line modules remain private descendants of their production parent, preserving existing private-parent access and exact discovery paths `ffi::codec_context::tests::*` / `track_mapper::tests::*`.
- Exact de-indented bodies matched HEAD before rustfmt; production prefixes matched HEAD exactly. `codec_context.rs` is 741 lines including the two-line test declaration. `track_mapper.rs` is still 917 lines and therefore remains an honest S42 follow-up.
- `scripts/module-size-baseline.json` was intentionally not changed: S42 reports codec baseline stale at 916 and track mapper legacy count changed from 1565 to 917; coordinated baseline reconciliation is deferred to the owner.
- Verification passed: focused 10/10 FFmpeg-feature, 6/6 no-feature and 27/27 mapper tests; full video-ffmpeg 88/88 feature plus raw FFI boundary, 60/60 no-feature plus raw boundary; full symphonia-demux 181/181; strict affected Clippy; owned rustfmt; tracked diff-check; Serena diagnostics.