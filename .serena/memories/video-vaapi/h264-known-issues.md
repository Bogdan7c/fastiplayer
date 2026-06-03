# H.264 status / known issues (validation 2026-06-03)

Разборы и планы: `user/h264_playback_perf_findings.md`, `user/h264_seek_bug_diagnosis.md`, `user/h264_seek_perf_implementation_plan.md`.

## Seek correctness: fixed / validated
- Старый баг stale DPB tail после seek-flush закрыт: seek 6s -> 30s в ручной release-проверке коммитится около demux `actual_ms`, не около pre-seek position.
- Первый post-seek presented frame не является stale H.264 DPB tail. `video-vaapi` seek-flush использует discard policy для tail events/decoder-owned ready frames; EOF/DPB drain остается отдельным intent и tail frames сохраняет.
- Player-side landing-frame guard остается обязательной защитой: seek frame может открыть gate/дать commit только для активной generation и `frame_pts >= seek_commit.actual_position`.
- VP9 seek/playback regression в ручной проверке не найден.

## Автоматическая validation wave 2026-06-03
- Passed before perf fix: `cargo test -p video-vaapi --lib`, `cargo test -p player-core seek --lib`, `cargo test -p codec-core h264 --lib`, `cargo test -p symphonia-demux --test h264_fixtures`, `cargo check --workspace`.
- Passed after perf fix: `cargo test -p video-vaapi --lib` (68 tests), `cargo test -p player-core seek --lib` (103 seek-filtered tests), `cargo test -p codec-core h264 --lib` (12 tests), `cargo test -p symphonia-demux --test h264_fixtures` (4 tests), `cargo check --workspace`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`.
- Clippy exit code was 0; remaining warnings are existing scope debt: patched `cros-*` cfg/lifetime warnings, `codec-core::h264` redundant closure call, `player-core` large enum / too_many_arguments.

## H.264 adapter perf/status
- Adapter perf changes remain validated by parser/conversion/keyframe tests and VAAPI adapter lifecycle tests: SPS/PPS injection is lifecycle/keyframe-driven, reusable Annex B scratch owns converted AU bytes, partial AU/backpressure semantics preserved.
- Debug 4K60 H.264 throughput root cause was unoptimized dev-profile CPU work in the hot H.264 adapter/parser/VAAPI submit path, not demux cadence, scheduler sorting, queue capacity, or render release. `Cargo.toml` now sets `[profile.dev.package.*].opt-level = 3` only for hot decode crates: `codec-core`, `video-vaapi`, `cros-codecs`, and `cros-libva`.
- This is a dev workflow fix only: release profile is unchanged; no runtime boundary/API changed; demux packet order/PTS/DTS, scheduler cadence/drop policy, seek flush discard, EOF drain, partial AU backpressure, decoded frame release, and render lease accounting remain unchanged.
- Manual debug validation after the profile fix: baseline 4K60 no-B MP4, Main+B MP4, High+B MP4, High+B+AAC MP4, and VP9 SDR all showed 0 late drops, stable present queue around 5-8, and no repeated full decoder packet-channel backpressure during steady playback. H.264 `decoder_submit_worst_ms` stayed roughly in the 17-23 ms range depending on sample instead of the previous 40-115 ms debug worst samples.
- Release manual validation after the profile fix: H.264 baseline MP4, High+B+AAC MP4, and High+B+AAC MKV stayed at 0 drops with stable present queue. The MP4 B-frame PTS/DTS fix remains the local `symphonia-format-isomp4` patch; do not revert it and do not compensate in scheduler sorting.
