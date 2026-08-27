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
- Whole-body transport был историческим bottleneck, но текущий committed HLS path использует bounded chunk-streaming и сохраняет strict resource validation/resource-bounded parser probe.
- Финальный release snapshot 2026-08-28 на clean committed HEAD подтвердил 3 cold InitialRestore requested `355.000 s` с actual `360.033 s` и process-to-ready `702/447/573 ms`; 10 warm seeks дали p50 `338 ms`, p95/max `1169 ms`. Residual `1169 ms` возник до receipt во внешней body delivery; post-receipt video/audio/commit завершились за `18/19/19 ms`, поэтому это не текущий parser/readiness regression и не долговечная CDN гарантия.
- Controlled real-profile acceptance восстановил state/resume byte-for-byte. Ephemeral backup location намеренно не хранится как durable project contract; каждый новый real-profile run создаёт и проверяет свежий recoverable backup.

## Проверка

- Affected media-core/demux-api/MPEG-TS/HLS/player/app suites, source/adaptive tests, strict all-target Clippy, workspace check, release build, rustfmt, diff-check, refactor/S42 guardrails, Python diagnostics/acceptance и Serena diagnostics прошли.
- Exact test counts не являются контрактом и намеренно не фиксируются: текущий результат следует читать из конкретного CI/reviewer run.
- Финальный HLS change закоммичен как `72a3cbf7`; worktree после real acceptance clean.

Related: `mem:mpeg-ts-demux/core`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:media-services/hls-preview-receipt-cancellation-2026-08-27`, `mem:core`.
