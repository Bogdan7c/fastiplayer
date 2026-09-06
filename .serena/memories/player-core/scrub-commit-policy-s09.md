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
- Live drag target hold/backpressure fix (2026-09-06): `mem:player-core/live-drag-target-hold-2026-09-06`. Во время удержания landing scheduler не применяет audio-stall recovery, decoder I/O сохраняет bounded queue, tiny forward extension переиспользует уже подходящий presented frame. Release policy не менялась.


## S13 playback-window уточнение (2026-07-20)
- Seek/LiveScrub входы и `timeline.target_position` остаются relative к playback window.
- Session переводит target в absolute source time ровно перед demux/decoder route; visible/live commit policy, generation gates и pending semantics не изменились.

## S31L dynamic live/DVR уточнение (2026-07-23)
- При sliding live window player повторно проверяет и active `SeekCommitState.target_position`, и public latest scrub target. Выпавший active route завершается typed `SeekTargetExpired`, даже если более новая pointer target ещё находится внутри DVR range.
- `CommitVisiblePreview` дополнительно проверяет timing показанного кадра против последнего DVR range. Выпавший preview получает `VisibleScrubPreviewUnavailableReason::OutsideLatestLiveRange` и сохраняет старую policy: exact fallback к валидной latest pointer target, без seek к просроченному кадру.

## Reused-decoder demux failure provenance (2026-08-13)
- `PlayerSession` остаётся владельцем пользовательской seek-диагностики: при terminal ошибке реального `demux.seek_with_request()` scrub lifecycle сначала классифицирует нейтральный `ScrubLifecycleError`, затем записывает исходный backend detail в `PlayerSnapshot.last_error` до cleanup route.
- `MediaDemuxError::is_seek_unavailable()` сохраняет `PlayerErrorKind::SeekUnavailable` для отсутствующего/неподдерживаемого seek; прочие terminal demux failures получают `PlayerErrorKind::DemuxError`. Конкретная причина больше не затирается общей фразой `SeekLanding не смог стартовать reused-decoder scrub route`.
- Regression `session/tests/seek_start.rs::reused_decoder_seek_preserves_terminal_demux_failure_in_player_snapshot` проходит через настоящий reused-decoder SeekLanding route и проверяет сохранение detail, очистку commit/scrubbing и возврат в `Paused`.
