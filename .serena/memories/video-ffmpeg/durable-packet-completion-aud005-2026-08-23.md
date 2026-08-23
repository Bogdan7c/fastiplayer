# AUD-005: durable FFmpeg packet completion accounting (2026-08-23)

## Статус и подтверждение

- AUD-005 независимо подтверждён на production `MpegTsDemuxer` + software FFmpeg worker и исправлен 2026-08-23.
- Accurate seek target: 2 s; фактический decode point: 1_000_000 us.
- При `packet_channel_frames = 1` прежняя ACK capacity была 1. Worker принял 16 H.264 packets; 14–15 завершались без немедленного output frame-а.
- При задержанном ACK consumer-е прежний bounded channel доставлял 1 completion, оставляя ложный player-style `in_flight = 15` после фактического terminal `VideoDecoderEndOfStreamDrainState::Drained`.
- Расхождение повторилось 5/5. Context7 подтвердил: `crossbeam_channel::Sender::try_send` возвращает `TrySendError::Full(T)`, и payload не отправляется.

## Корень проблемы

`crates/video-ffmpeg/src/decoder_thread.rs` использовал bounded `packet_ack_tx/rx` с capacity, равной packet channel, и worker выполнял `let _ = packet_ack_tx.try_send(1)`. Обязательная accounting-истина была ошибочно объединена с best-effort notification transport. Увеличение capacity только отодвигает потерю; blocking `send` способен заблокировать единственный FFmpeg owner thread и вместе с ним control/flush/EOF lifecycle.

## Исправленная граница

- `FfmpegDecoderWorker` остаётся владельцем фактического момента packet completion.
- `FfmpegPacketCompletionCounter` — shared durable atomic accumulator между worker и playback-facing handle.
- После `progress_report.packet_completed` worker вызывает `record_completion()`; bounded ACK send удалён.
- `VideoDecoderThreadHandle::drain_completed_packet_count()` сохраняет прежний публичный/internal contract и вызывает atomic `swap(0)`.
- Concurrent increment попадает либо в текущий drain, либо в следующий; completion не теряется и не передаётся дважды.
- `VideoDecoderActivityNotifier` остаётся coalesced wake-up hint и не является accounting source of truth.
- Seek generation, frame release, error states, EOF states и neutral decoder API не менялись.
- `Ordering::Relaxed` достаточен: атомик синхронизирует только независимое числовое accounting; frame/error/EOF payload-и имеют собственные существующие boundaries.

## Regression anchor

`crates/video-ffmpeg/tests/aud005_packet_ack_loss.rs::accurate_seek_eof_preserves_all_packet_completions` — ignored real regression с explicit `RUSTIPLAYER_MEDIA_PATH`.

Тест:

1. открывает generated MPEG-TS через production demux;
2. делает decode-safe accurate seek;
3. запускает настоящий software FFmpeg worker с `packet_channel_frames = 1`;
4. прогоняет 16 packets и намеренно не дренирует completion accumulator;
5. корректно отличает generic Deferred activity от фактического completion по FFmpeg worker activity state machine;
6. освобождает materialized frames через normal `release_frame`;
7. доводит decoder DPB/EOF до `Drained`;
8. требует `accepted == delivered_completions`, terminal `in_flight == 0` и повторный drain `0`.

Post-fix real result: `accepted=16`, `delivered_completions=16`, `terminal_in_flight=0`, `no_output_packets=14`, `actual_seek_us=1000000`.

Fixture generation:

```bash
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=160x90:rate=30 \
  -t 4 -c:v libx264 -preset ultrafast -profile:v baseline \
  -bf 0 -g 30 -keyint_min 30 -sc_threshold 0 -pix_fmt yuv420p -an \
  -muxpreload 0 -muxdelay 0 -mpegts_flags +resend_headers \
  -f mpegts -y /tmp/rustiplayer-aud005-ack-loss.ts
```

Real regression command:

```bash
env RUSTIPLAYER_MEDIA_PATH=/tmp/rustiplayer-aud005-ack-loss.ts \
  cargo +1.96.0 test -p video-ffmpeg --features ffmpeg --locked \
  --test aud005_packet_ack_loss -- --ignored --exact \
  accurate_seek_eof_preserves_all_packet_completions --nocapture
```

## Проверки

- Real AUD-005 regression: 1/1 PASS.
- `cargo +1.96.0 test -p video-ffmpeg --features ffmpeg --locked`: 87 unit tests + integration compile/run PASS; real tests ignored by default.
- `cargo +1.96.0 test -p player-core --locked`: 643/643 PASS, включая существующий functional EOF/Ended lifecycle.
- `cargo +1.96.0 clippy -p video-ffmpeg --features ffmpeg --all-targets --locked -- -D warnings`: PASS.
- `cargo +1.96.0 fmt --all --check`: PASS.
- Serena diagnostics: только ожидаемый inactive-code hint для противоположной cfg-ветки; ошибок/предупреждений нет.

## Ограничение

Real regression требует explicit generated local MPEG-TS и system FFmpeg libraries, поэтому он ignored в hermetic default suite. Репрезентативная частота прежнего дефекта на пользовательском corpus не измерялась; correctness invariant закрыт детерминированным stress path-ом.