# Передача времени показа монотонным часам после конца/разрыва звука (gles2 сессия 13b, решение 92, 2026-10-28)

Отчёт: `user/gles2-renderer/results/13b.md` (раздел «Находка: файл не заканчивается…»).

## Дефект (давний, найден живьём на T410)
Время показа вели только аудиочасы (`session/position_clock.rs`, пока `has_audio_clock()`). Когда звук доигран или пропал, часы вставали:
- EOF-drain вечно ждал `VideoPresentQueue` (seek за последний кадр → `eof-fallback-frame`; звук на 1–2 с короче видео);
- в `Playing` — взаимная блокировка: decode-ahead (`tick/video_decoder_io/decode_ahead.rs`) держит видео по вставшим часам, demux audio-priority дочитывает `max_pending_video_packets_during_audio_catchup` (240) и ждёт звук (`WaitingForDemuxAudioPriority`);
- пауза в звуке посреди файла без блокировки → склейка (звук 12 с звучал на 5 с).

## Архитектура
- Владелец — `PlaybackPipeline`, `pipeline/presentation_clock_handover.rs`: `PresentationClockSource::{AudioOutput, MonotonicAfterAudioEnd, MonotonicWithoutAudio}` (`presentation_clock_source()`), `hand_over_presentation_clock_after_audio_end` (хвост в EOF-drain), `hand_over_presentation_clock_for_audio_gap` (разрыв в Playing), `resume_audio_clock_after_gap(M)`, `cancel_audio_end_clock_handover`; `note_audio_packet_written` / `written_audio_end`, `front_pending_audio_packet_start`. Исходы раздельные (`AudioEndClockHandoverOutcome`, `AudioGapResumeOutcome`).
- Session: `session/video_tail_clock_handover.rs` (из `eof_drain::finish_eof_drain_if_ready`, когда blocker — хвост видео, `EofDrainBlocker::waits_for_video_tail`); `session/audio_gap.rs` — доказательства (следующий audio packet позже `written_audio_end` + `MIN_AUDIO_GAP`=500 мс; demux audio-priority упёрся в лимит без audio packets, Playing, звук доигран), удержание звука `PendingAudioOutputAdmission::HoldForAudioGap`, возврат `resume_audio_after_gap_if_due`; `tick/audio_gap_evidence.rs`.
- Выбор часов, stall (`audio_stall.rs`), decode-ahead, demux catch-up (`audio_demux_catchup_needed`), pause/freeze (`runtime_control::presentation_position_to_freeze`), `playback_rate.rs` — через `audio_clock_drives_presentation()`.
- Сброс передачи: `begin_seek_generation`, `reanchor_audio_clock_media_mapping` (и всё, что через него), install/clear/reset audio clock, `reset_media_slots`, повтор после EOF.
- `queued_video_frames_unreachable_after_audio_end` оставлен: кадры с PTS ≥ duration не держат drain.

## Уточнение 12b (2026-10-28): насыщение demux — доказательство только «за концом звука»
- Живой дефект T410: HW H.264 на 2×/4×, видео отстаёт → очередь 240 video packets лежала ДО конца записанного звука, `DemuxSaturatedWithoutAudio` ложно отдавал часы monotonic clock, звук обрезался и не возвращался до seek.
- Теперь `audio_gap.rs::demux_read_video_far_past_written_audio`: насыщение доказывает отсутствие звука, только если `PlaybackPipeline::furthest_pending_video_packet_start` (максимальный PTS очереди выбранного трека/поколения, B-frames) > опора + `MIN_AUDIO_GAP`; опора = `written_audio_end`, а после seek (конец звука сброшен) — `snapshot.current_position` (иначе seek в область без звука снова вечно блокируется).
- Тесты: `lagging_video_decoder_with_continuous_audio_never_hands_the_clock_over`, `seek_into_region_without_audio_hands_the_clock_over_when_demux_saturates`, `seek_at_4x_into_audio_region_with_lagging_decoder_keeps_audio_clock`; юнит `furthest_pending_video_packet_start_reports_demux_frontier_of_selected_track`.

## Тесты
`session/tests/eof_audio_ends_before_video.rs` (7), `session/tests/audio_gap_clock_handover.rs` (4, вкл. «редкие audio packets → нет ложной передачи»), `pipeline/tests/presentation_clock_handover.rs` (10). Мутации описаны в отчёте.

## Ограничения
Разрывы 20–500 мс с 03e закрываются тишиной (см. `mem:player-core/audio-runtime` «Дыра посреди отрезка»); звук, отстающий от видео больше лимита catch-up, обрежется как прошедший (раньше — вечная блокировка); seek внутрь паузы звука не тестирован; живьём финальная сборка проверена на 780M, T410 (GLES2, Wayland/XWayland) и T480s (wgpu/GLES2): все случаи доходят до конца и следующего файла, возврат звука после паузы — 6 мс. Во время паузы звука раз в 2 с WARN `Audio output starvation observation` (шум).
