# Session 09: real ScrubCommitPolicy semantics (2026-07-11)

- Public `ScrubCommitPolicy` modes are both executed by `PlayerSession::end_scrub`; the old `policy: _` dispatch is removed.
- `CommitLatestTarget` selects an exact commit to the resolved latest pointer `SeekRequest`.
- `CommitVisiblePreview` selects the last live-scrub frame that the player-owned presentation scheduler actually made the current presented frame. The UI does not recompute position.
- `SeekRuntimeState` owns `VisibleScrubPreview { context, timing, frame_identity }`. It is recorded from `note_presented_frame_for_seek` only for the active LiveScrub route and matching decoder generation.
- A visible candidate is accepted only when its source/backend/playback/scrub guards, track selection, active target context, timing, and full `VideoPresentFrameIdentity` still match the current route/presented frame.
- Invalid or missing preview always uses exact latest-target fallback; there is no no-op/current-landing choice. Public `ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget` carries a typed `VisibleScrubPreviewUnavailableReason`.
- `PlayerCommandOutcome::ScrubCommit` exposes the policy resolution separately from asynchronous SeekLanding completion. Worker dispatch logs the typed outcome and preserves existing fatal/recoverable handling.
- Valid visible preview and latest pointer target can intentionally differ: visible policy opens Accurate SeekLanding at the visible frame media timing, while latest policy opens/continues exact SeekLanding at the pointer target.
- Timeline release intent is gesture-specific after the click regression fix: a short click that temporarily enters live scrub ends with `CommitLatestTarget`, so the clicked coordinate is not replaced by the first visible/keyframe preview; an actual drag release still ends with `CommitVisiblePreview` to avoid a visual jump. `app-egui::ui::timeline::TimelineAction` keeps these intents distinct as `EndLiveScrubAtLatestTarget` and `EndLiveScrubAtVisiblePreview`, and `state::ui_runtime` alone maps them to player-core policy.
- Existing SeekLanding remains the sole lifecycle owner: decoder flush/generation, demux seek, current-frame hold/release, audio resume, final exact gates and clock commit are not duplicated in UI or command code.
- Focused coverage in `crates/player-core/src/session/tests/scrub.rs`: missing preview, policy-specific simple fallback, valid visible-vs-latest targets, stale scrub generation, source/backend/track mismatch, active live reuse, and decoder flush error.
- Verified with `cargo test -p player-core` (478 tests), `cargo test -p app-egui` (241 tests), locked workspace check, strict Clippy for player/app, fmt, and refactor guardrails.
- Related base knowledge: `mem:player-core/core`, `mem:frame-server/core`.


## S13 playback-window уточнение (2026-07-20)
- Seek/LiveScrub входы и `timeline.target_position` остаются relative к playback window.
- Session переводит target в absolute source time ровно перед demux/decoder route; visible/live commit policy, generation gates и pending semantics не изменились.
