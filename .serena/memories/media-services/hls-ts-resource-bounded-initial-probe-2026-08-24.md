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
- Реальный GUI до исправления воспроизвёл `SeekUnavailable: Demux worker не смог выполнить seek` и возврат к 0. После исправления resume с 355.251 s принял anchor 350.033 s примерно за 0.2 s, и video дошло до render/play. Повторный seek также остался `Playing`. Холодный restart с сохранённым 630.244 s поднял HLS, decoder/audio/render и MPRIS `Playing` около 634.159 s.
- После acceptance пользовательские `playlist-state.json` и `playlist-resume.json` восстановлены к исходным item 2 / 355.251162841 s.

## Проверка

- `cargo +1.96.0 test -p media-core -p demux-api -p mpeg-ts-demux -p web-media-hls -p player-core --all-targets --locked`: PASS; MPEG-TS 39, player-core 649, все HLS unit/integration targets.
- `cargo +1.96.0 test -p app-egui --no-default-features --locked`: 973/973 PASS.
- Strict affected Clippy `-D warnings`, workspace all-target check, rustfmt, diff-check, refactor guardrails и Serena diagnostics: PASS.

Related: `mem:mpeg-ts-demux/core`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:core`.
