# HLS MPEG-TS resource-bounded initial probe (2026-08-24)

## Реальный дефект

- Источник: `https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8`, пользовательский checkpoint `355.251162841 s`.
- Worker-receipted manifest seek правильно выбрал target segment `url_625/193039199_mp4_h264_aac_fhd_7.ts`, но `MpegTsDemuxer::open` вернул malformed: PID 257, `PES packet_length` больше собранных bytes.
- Сегмент валиден. AAC PID 257 имеет длинный declared PES, payload которого перемежается множеством video TS packets. Default initial probe останавливался после 4096 × 188 bytes, то есть в середине AAC PES. `finish_pending_elementary_streams(false)` корректно fail-closed трактовал оборванный PES как corruption, хотя оборван был bounded probe, а не resource/EOF.
- Поэтому прежний diff исправлял нужный seek routing и manifest restart, но не owner-local parser cutoff. Удобные synthetic segments и некоторые реальные seek targets завершали PES раньше 4096 packets и скрывали regression.

## Ownership и API

- `mpeg-ts-demux::MpegTsDemuxOptions::with_initial_probe_byte_budget(NonZeroUsize)` — additive named API. MPEG-TS owner переводит byte budget в число целых 188-byte packets с округлением вверх.
- Default `initial_probe_packets = 4096` не изменён для generic/local/stream inputs.
- App HLS composition получает уже validated `AdaptiveTransportLimits::maximum_segment_bytes` из `network.memory_cache_mb` и только для HLS factory передаёт его как initial probe budget.
- HLS и app не знают внутренние поля parser-а; MPEG-TS не знает network config. Один и тот же resource owner bound ограничивает download/retention и допустимый topology probe.
- Остальные limits (`pes_bytes`, AU, resync, index/seek scan) не меняются. Corrupted/truncated resource по-прежнему fail-closed.

## Regression evidence

- Owner test `resource_bounded_probe_reaches_interleaved_audio_evidence_beyond_default_cutoff` создаёт muxed H.264/AAC segment, где полный AAC PES появляется после прежнего cutoff. Default registry обязан воспроизвести старый open failure; resource-bounded registry обязан открыть video+audio tracks и выдать packets обеих дорожек.
- HLS integration `late_receipted_seek_fetches_target_segment_and_publishes_landing_packet` использует такой target segment, получает authoritative receipt на 60 s, не fetch-ит промежуточные segments и публикует post-seek video RAP.
- Player functional regression `worker_receipted_video_seek_reaches_target_frame_presentation` доводит receipt через decoder до presentation target-frame.
- Реальный GUI до parser fix воспроизвёл `SeekUnavailable: Demux worker не смог выполнить seek` и возврат к 0; после fix authoritative anchor снова стал доказуемым. Прежний единичный receipt около 0.2 s был прогретым CDN и не является performance evidence.
- Исторический latency re-audit до streaming implementation показал full-body transport gate: 10 warm seek дали p50 около 0.993 s и max 1.324 s, rapid supersede 1.673 s, а худший cold resume дошёл до frame за 20.288 s. Эти измерения относятся к прежнему whole-body path и не являются acceptance текущего worktree.
- Текущий незакоммиченный HLS TS path использует bounded chunk-streaming и сохраняет strict resource validation/resource-bounded parser probe. После source-scoping policy real release acceptance подтверждён: 3 cold resume 355 s достигли actual 360.033 s и full video/audio/startup readiness за 447–547 ms; 10 warm seek дали p50 35 ms, p95/max 295 ms. Полная архитектура, матрица и известный S42 gate описаны в `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`.
- Исторический re-audit сообщал успешное восстановление пользовательских `playlist-state.json` и `playlist-resume.json` к item 2 / 41.401578841 s. Временный backup path намеренно не хранится как durable project contract; каждый новый real-profile acceptance обязан создать и проверить свежий backup.

## Проверка

- `cargo +1.96.0 test -p media-core -p demux-api -p mpeg-ts-demux -p web-media-hls -p player-core --all-targets --locked`: PASS; media-core 66, demux-api 59, MPEG-TS 43, player-core 669, HLS unit 59 и все integration targets.
- `cargo +1.96.0 test -p app-egui --no-default-features --locked`: 1000/1000 PASS.
- Source/adaptive tests, strict affected all-target Clippy `-D warnings`, workspace all-target check, release build, rustfmt, diff-check, refactor guardrails, Python diagnostics/acceptance и Serena diagnostics: PASS.
- Канонический `scripts/pre-pr-checks.sh` доходит до S42 и останавливается на накопленном module-size baseline большого незакоммиченного worktree; не маскировать это обновлением baseline в focused HLS fix.
Related: `mem:mpeg-ts-demux/core`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:core`.
