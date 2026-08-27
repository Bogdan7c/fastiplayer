# S42 playback executor Wave 3 private production-module decomposition (2026-08-27)

Behavior-neutral ownership split at HEAD f40ee56a; no public API, lifecycle, error-state, or test assertion changes.

- `crates/player-core/src/session/seek_start.rs` is 779 lines and owns seek transaction ordering, generation, landing, and resume intent. Its private child `session/seek_start/error_mapping.rs` is 40 lines and owns only the three typed conversions for demux seek unavailable vs generic demux error, unsupported seek mode, and decoder flush failure. Typed demux/flush distinctions and seek generation/resume semantics remain unchanged.
- `crates/video-ffmpeg/src/decoder_thread/send_receive.rs` is 720 lines after formatting and still owns the FFmpeg send/receive state machine, packet completion, EAGAIN, EOF, and flush/drain lifecycle. Its private child `decoder_thread/send_receive/timestamps.rs` is 179 lines and owns track PTS/DTS selection, packet time-base conversion, saturating unit conversion, and best-effort/PTS/interpolation frame timestamp policy. `NO_TIMESTAMP` remains parent-visible for existing decoder-thread tests. Production-only imports remain feature-gated, so no-feature builds stay warning-free.
- `crates/video-vaapi/src/decoder_thread/runtime_loop.rs` is 636 lines and still owns control scheduling, disconnect handling, flush/reconfigure, and EOF drain gates. Its private child `decoder_thread/runtime_loop/frame_publication.rs` is 277 lines and owns pending decoded-frame publication, pressure diagnostics, decode-outcome ACK/backpressure mapping, channel disconnect retention, and exactly-once pending-frame release. Items are restricted to the existing `decoder_thread` scope and re-exported privately so existing focused tests keep their paths.

Verification:
- focused player seek tests 17/17;
- focused FFmpeg decoder-thread tests 29/29;
- focused VAAPI decoder-thread tests 24/24;
- combined `cargo test --locked --all-features -p player-core -p video-ffmpeg -p video-vaapi`: player-core 669/669, video-ffmpeg 88/88 plus raw FFI boundary 1/1, video-vaapi 160/160; explicit fixture/hardware tests remain ignored by their existing contracts;
- FFmpeg no-feature 60/60 plus raw FFI boundary 1/1;
- strict all-targets all-features Clippy for all three crates and strict no-feature video-ffmpeg Clippy passed;
- targeted rustfmt, whitespace/diff checks, and final Serena diagnostics for all six owner files passed.

The repo-wide S42 guardrail still requires coordinated baseline reconciliation. This slice changes the expected runtime-loop snapshot from legacy 891 to 636; `scripts/module-size-baseline.json` was intentionally not edited by the slice owner. Repo-wide `cargo fmt --all --check` was blocked only by concurrently owned source-core/web-media-dash formatting, while all six owned files passed targeted rustfmt check.
