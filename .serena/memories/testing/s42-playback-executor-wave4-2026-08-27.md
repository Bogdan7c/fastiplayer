# S42 playback executor Wave 4 private production-module decomposition (2026-08-27)

Behavior-neutral ownership split from HEAD `36447fb2`; no public API, runtime lifecycle, queue/accounting, timing, cancellation, or typed error outcome changed.

- `crates/video-vaapi/src/decoder_thread.rs` is 760 lines and remains the owner of bounded channel construction, thread spawn, the public decoder-thread handle, control/send/receive orchestration, resource release, flush/EOF-drain lifecycle, and runtime-loop attachment. Its private child `decoder_thread/config.rs` is 518 lines and owns channel/default configuration, env timeout parsing/normalization, public config/packet/error DTOs, neutral video-core projections, resource snapshot projections, and the shared frontend sticky-fatal latch. Existing public items are re-exported from the unchanged `decoder_thread` path; the child module is private.
- `crates/player-core/src/session/tick/presentation_scheduler.rs` is 746 lines and remains the owner of presentation queue mutation, pop/drop/release/present/repeat, seek-preroll execution, adaptive catch-up execution, and decoder/demux work orchestration. Its private child `presentation_scheduler/timing.rs` is 504 lines and owns read-only scheduler diagnostics, queue/admission limit projections, audio/presentation clock target math, present lead/window, late-drop grace decisions, scheduler wake delay, and pure adaptive catch-up budget calculations. Internal visibility is restricted to `crate::session::tick`, matching the old parent-module boundary.
- `crates/player-core/src/session/staged_media_install.rs` is 738 lines and remains the owner of admission, no-overwrite/supersede ordering, audio/video planning ingress, detached backend preparation, Ready/position/commit protocol, cancellation and typed resource/backend/status mapping. Its private child `staged_media_install/preflight.rs` is 242 lines and owns the exact request/generation fence, pending continuation, bounded registry slot/tombstone, retry/timeout wake calculation, and terminal preflight progression. Registry visibility is restricted to `crate::session`; timeout, cancellation, unsupported/configuration/backend failures and exactly-once terminal behavior are unchanged.

Verification:
- focused VAAPI decoder-thread tests: 24/24;
- focused player scheduler/tick tests: 88/88, including accurate preroll, audio-clock mapping, scheduler delay, late-drop grace and adaptive catch-up;
- focused staged install tests: 27/27, including typed timeout, supersede/shutdown cancellation, backend/resource/configuration failures, old-playback preservation and exact-once terminal resolution;
- combined `cargo test --locked --all-features -p player-core -p video-vaapi`: player-core 669/669 and video-vaapi 160/160, doc tests clean;
- strict `cargo clippy --locked --all-features --all-targets -p player-core -p video-vaapi -- -D warnings` passed;
- targeted rustfmt, repo-wide `cargo fmt --all --check`, `git diff --check`, and `scripts/check-refactor-guardrails.py` passed;
- Serena diagnostics are clean for all six owner/child files after project reactivation refreshed the new-module index.

Existing functional tests already exercise the moved production boundaries end-to-end, so no helper-only tests were added. The module-size baseline was intentionally not edited by this slice owner.