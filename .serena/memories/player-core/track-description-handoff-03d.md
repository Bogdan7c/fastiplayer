# Смена описания дорожек без сброса — «эстафета» (сессия 03d моста, 2026-10-30)

Решения владельца 105–109 (`user/browser-bridge/README.md`, отчёт `results/03d.md`). Общее правило плеера для всех источников (мост, HLS, DASH, MPEG-TS), а не особенность моста.

## Механизм
- `TracksChanged` без активной перемотки/скраба → `PlayerSession::apply_track_list_update_without_reset` (`session/track_transition.rs`): список дорожек и длительность обновляются сразу (`pipeline.replace_track_list_keeping_decoders`), изменённое ОПИСАНИЕ выбранного видео/звука (codec_id, codec_private, time_base, sample_rate, channels, `video`; НЕ duration/labels) получает номер в `TrackDescriptionLedger` (`pipeline/track_description.rs`). Пакеты после этого помечены номером (`with_track_description` в `tick/demux_admission.rs`).
- Видео: `pipeline.front_sendable_video_packet()` — «шлагбаум»: пакет новее описания декодера для send loop (`video_decoder_io.rs`) и планировщика пробуждений (`wakeup.rs`) не существует. `tick/video_track_handoff.rs::advance_video_track_handoff` (раз в tick из `presentation_scheduler::process_pending_video_packets`, до decoder IO): забрать ACK-и → ждать in_flight == 0 → EOF/DPB drain текущей generation → ждать `Drained` → `flush` → `reconfigure_video_decoder_for_track` (`capability_selection/track_reconfiguration.rs`: `Configured` / `ControlChannelBusy` → повтор / `BackendReselectionRequested` → шлагбаум закрыт, перемотка нового backend-а сама настроит) → `require_video_decoder_keyframe` → `complete_video_track_handoff`. Generation НЕ меняется, очередь показа и звук не трогаются.
- Звук: `apply_audio_track_description_for_packet` в `process_pending_audio_packets_with_buffer_limit` — на первом пакете нового номера декодер заменяется deferred-конфигом нового описания; выход/часы те же (смена формата PCM — `audio_spec_change`).
- Seek flush: `reset_video_decoder_for_seek` (seek_start.rs) после flush вызывает `configure_video_decoder_for_latest_track_description` — `settle_video_track_descriptions_after_flush` + `select_default_video_track` (без выжимания, до preroll floor).

## Старый сброс (`handle_demux_track_list_update`) остаётся
Активный seek commit / ждущая квитанция / live-scrub; ждёт смена backend-а; другой состав видео/аудио (id+kind по порядку) или выбор не согласован со списком; активный декодер не принимает описание (`active_video_decoder_accepts_track`); описание звука не строится; > `MAX_PENDING_TRACK_DESCRIPTIONS` (4) ждущих. Учёт сбрасывается в `install_opened_media`, `reset_media_slots`, `apply_demux_track_list_update`.

## Инварианты
- Ни один кадр до отметки не выбрасывается; ни один пакет нового описания не уходит старому декодеру.
- После передачи `flush` возвращает drain state в `Idle` (договор `mem:video-core/decoder-stream-boundary`), поэтому настоящий EOF drain той же generation проходит заново.

## Тесты
`session/tests/track_transition.rs` (10, подставной декодер по тактам), `pipeline/tests/track_descriptions.rs` (9), `session/track_transition/classification_tests.rs` (8); сквозные `pushed-media-source/tests/stream_changes_to_frame.rs` (все 20 кадров, 64→96 ровно на 0,4 с, звук без тишины и без выброшенного буфера, звук далеко впереди → `Ended`, H.264→VP9).

## Ограничения
Во время seek/scrub — старый сброс; смена backend-а — пересоздание + перемотка; ошибка flush фатальна (как у seek); новый эпизод не с ключевого кадра → пропуск до ключевого; декодер звука меняется без выжимания хвоста, passthrough не перевыбирается. В GUI вживую эстафету сейчас не на чем увидеть: HLS TS меняет SPS внутри потока без `TracksChanged`, fMP4 HLS и склеенный TS со сменой кодека не открываются (общий бэклог 27, 28).
