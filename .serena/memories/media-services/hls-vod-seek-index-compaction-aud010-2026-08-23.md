# AUD-010 — bounded whole-timeline HLS VOD seek index (2026-08-23)

## Подтверждённый дефект

- Независимая read-only сессия на production HLS transport + MPEG-TS demux ограничила muxed A/V index четырьмя entries и прочитала шесть 30-секундных segments.
- Старый `HlsSeekIndex::observe_packet` после заполнения молча прекращал принимать anchors. Seek `150 s` выбрал старый anchor `30 s` и последовательно повторно fetch-нул пять media segments вместо одного.
- Seek replacement после positioning снова подключал старый shared index, поэтому состояние не восстанавливалось.
- Production app budget остаётся `4096`; отдельные audio/video renditions имеют независимые component indexes, muxed A/V хранит оба вида в одном index.

## Новый owner invariant

- `crates/web-media-hls/src/seek.rs::HlsSeekIndex` единолично владеет compaction; app, epoch demux и player-core не знают структуру хранения.
- Общий caller-owned budget динамически делится между video RAP и audio anchors. Video получает нечётный приоритет только при дефиците; неиспользованная доля одного вида доступна другому.
- При budget >= 2 для вида сохраняются первый и самый свежий anchors. Внутренние anchors выбираются около равномерных временных целей по всей доказанной timeline.
- При budget == 1 сохраняется свежий anchor; при конкуренции video RAP приоритетен, потому что DecodePointBefore/Preview требуют RAP, а Accurate умеет fallback на video.
- Index всегда `len <= maximum_entries`, поздняя граница продолжает двигаться, per-segment/kind duplicate coalescing сохраняется.
- Preview pin хранит exact anchor по значению и переживает eviction/compaction без изменения latest-only worker semantics.

## Live compatibility

- Static VOD index не используется live/DVR.
- Live owner остаётся `HlsLiveComponentSnapshot` + `HlsLiveTimelineEvidence`, хранит только admitted retained segment identities и удаляет expired evidence при sliding refresh.
- Refresh cadence, live edge, ENDLIST drain, dynamic timeline и endpoint recovery не менялись; все девять `live_runtime` regressions прошли.

## Regression anchors

- Focused unit tests в `crates/web-media-hls/src/seek.rs`: A/V fairness and endpoints, intermediate spread, budget 1, no unused capacity, segment coalescing, preview pin after actual compaction.
- `crates/web-media-hls/tests/seek_index_compaction_runtime.rs::late_seek_after_tiny_index_compaction_restarts_directly_from_latest_segment`: six TS segments, limit 4, seek 155 s -> actual RAP 150 s -> exactly one refetch of segment 5 -> landing keyframe.
- Downstream functional contracts:
  - `player-core::session::tests::capability_selection::late_hls_h264_track_reaches_presentation_after_backend_install`;
  - `player-core::session::tests::eof_drain::seek_near_eof_with_video_and_audio_tail_reaches_auto_next_terminal_state`.
  Они проводят HLS/post-seek packets через decoder и presentation scheduler.

## Проверка

- `cargo +1.96.0 test -p web-media-hls -p player-core --all-targets --locked` PASS: player-core 643; HLS unit 42; AES/catalog/live/runtime/compaction targets PASS.
- `cargo clippy -p web-media-hls --all-targets --locked -- -D warnings` PASS.
- `cargo +1.96.0 check --workspace --locked` PASS.
- fmt, `git diff --check`, refactor guardrails PASS.

Related memories: `mem:core`, `mem:media-services/hls-vod-s32b-2026-07-23`, `mem:media-services/hls-vod-s32c-2026-07-23`, `mem:media-services/hls-live-s33-2026-07-24`, `mem:testing/hls-ts-vod-runtime-fix-2026-08-04`.