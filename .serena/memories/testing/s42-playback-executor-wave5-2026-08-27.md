# S42 playback executor Wave 5 private production-module decomposition (2026-08-27)

Behavior-neutral ownership split from HEAD `1a7c4da3`; no public API, decoder protocol, selection/reselection state, queue/accounting, generation/drop, release lifecycle, DMA-BUF format, HDR or typed error outcome changed.

- `crates/player-core/src/session/capability_selection.rs` is 677 lines and remains the owner of mutable session capability state, track activation, active backend filtering/reselection, decoder reconfiguration and position restoration. Its private child `capability_selection/requirement.rs` is 290 lines and owns pure track metadata -> `VideoDecodeRequirement` / `VideoStreamDecodeConfig` projection, packet-refinement eligibility, NV12/P010 fallback contract selection, and exact neutral capability/config rejection -> typed player-error mapping. The old sibling-visible functions remain re-exported from the unchanged `session::capability_selection` path.
- `crates/player-core/src/session/tick/video_decoder_io.rs` is 865 lines and remains the owner of decoder send/drain orchestration, present/admission budgets, in-flight completion accounting, decoded-frame enqueue/drop and exactly-once frame release. Its private child `video_decoder_io/packet_validation.rs` is 390 lines and owns pending packet codec/private-data probe, refinement outcome, keyframe/decode-start framing, stale-generation classification and typed DecoderReady / BackendReselectionPending / Rejected decisions. Existing tick-visible bootstrap/generation/capacity functions remain re-exported from the unchanged `video_decoder_io` path. Generation mismatch remains a nonfatal stale drop, and packet accounting still increments only after an accepted decoder send.
- `crates/video-vaapi/src/decoder.rs` is 1162 lines and remains the owner of adapter, surface/frame/resource pools, release/reclaim, decode/retry/event drain, preroll, flush/EOF and Drop lifecycle. Its private child `decoder/surface_contract.rs` is 185 lines and owns typed decoded surface contract, fatal zero-copy contract violation, VA stream/RT format mapping, DMA-BUF layout projection and frame-contract validation. NV12 8-bit 4:2:0, P010 10-bit 4:2:0, P010/I010 aliasing, SeparateLayers/ComposedLayers and fail-closed 12-bit/non-DMA-BUF behavior are unchanged.

Focused verification:
- capability requirement tests: 4/4;
- packet refinement tests: 2/2;
- VAAPI decoder contract tests: 8/8.

Combined verification:
- `cargo test --locked --all-features -p player-core -p video-vaapi`: player-core 669/669, video-vaapi 160/160, doc tests clean;
- `cargo clippy --locked --all-features --all-targets -p player-core -p video-vaapi -- -D warnings`: passed;
- `cargo +1.96.0 test -p service-ytdlp --test final_acceptance_s42 --locked`: 24/24;
- targeted rustfmt, repo-wide `cargo fmt --all -- --check`, `git diff --check`, and `scripts/check-refactor-guardrails.py`: passed;
- Serena diagnostics are clean for all six owner/child files after indexing the new modules.

Existing production-boundary functional tests cover active backend filtering/reselection/refinement, H.264/H.265 packetization and presentation, seek-generation drops, keyframe bootstrap, decoder admission/accounting, fatal decoded-frame mismatch tail release, VAAPI pool/reclaim/preroll and NV12/P010 contract paths; no helper-only behavior was substituted for those regressions.