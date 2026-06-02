# H.264 status / known issues (validation 2026-06-02)

Разборы и планы: `user/h264_playback_perf_findings.md`, `user/h264_seek_bug_diagnosis.md`, `user/h264_seek_perf_implementation_plan.md`.

## Seek correctness: fixed / validated
- Старый баг stale DPB tail после seek-flush закрыт: seek 6s -> 30s в ручной release-проверке коммитится около demux `actual_ms`, не около pre-seek position.
- Первый post-seek presented frame не является stale H.264 DPB tail. `video-vaapi` seek-flush использует discard policy для tail events/decoder-owned ready frames; EOF/DPB drain остается отдельным intent и tail frames сохраняет.
- Player-side landing-frame guard остается обязательной защитой: seek frame может открыть gate/дать commit только для активной generation и `frame_pts >= seek_commit.actual_position`.
- VP9 seek/playback regression в ручной проверке не найден.

## Автоматическая validation wave 2026-06-02
- Passed: `cargo test -p video-vaapi --lib` (68 tests), `cargo test -p player-core seek --lib` (103 seek-filtered tests), `cargo test -p codec-core h264 --lib` (12 tests), `cargo test -p symphonia-demux --test h264_fixtures` (2 tests), `cargo check --workspace`.
- `cargo clippy --workspace --all-targets` завершился с exit code 0. Остались warnings: patched cros crates (`unexpected_cfgs`, lifetime syntax) плюс project warnings (`codec-core::h264` redundant closure call, `player-core` large enum / too_many_arguments). Они не были исправлены в validation scope.

## H.264 adapter perf/status
- Adapter perf changes validated by parser/conversion/keyframe tests and VAAPI adapter lifecycle tests: SPS/PPS injection is lifecycle/keyframe-driven, reusable Annex B scratch owns converted AU bytes, partial AU/backpressure semantics preserved.
- Debug build still не тянет 4K60 H.264 при свободных ресурсах на любых H.264 samples; treat as debug-only perf limitation, not current release correctness regression.
- Release manual playback: seek stable, frame drops = 0, VP9 unchanged. Perfect smoothness observed for `h264_baseline_l52_160mbps_no_bframes_video_only.mp4`, `h264_high_l52_180mbps_bframes_aac.mkv`, and VP9.
- Remaining open issue: release MP4 high-bitrate B-frame samples show subjective ~30fps/microstutter despite 0 drops: `h264_main_l52_160mbps_bframes_video_only.mp4`, `h264_high_l52_180mbps_bframes_video_only.mp4`, `h264_high_l52_180mbps_bframes_aac.mp4`. Root cause not diagnosed; likely next investigation should compare MP4 timestamp/reorder cadence, presentation scheduling/repeat accounting, and renderer/vsync pacing before changing decoder boundaries.
