# Same-lineage reopen у хвоста/конца media (backlog #21, 2026-10-17)

Симптомы D6 (смена web-версии звука AAC↔E-AC-3 в HLS): отказ `staged demux anchor lies after authoritative target` или вечный `WaitingForVideoTargetFrame`. Корень — не «последний сегмент», а пять мест staged reopen/EOF в player-core (HLS не менялся).

## Корни и фиксы
1. **Redundant `SelectVideoTrack`** (app `InstalledMediaStateRestore` всегда пере-выбирает прежние дорожки): `select_requested_video_track` заново выводил requirement из container metadata (беднее preflight-config-а) → VA-API `configure_stream` пересоздавал adapter посреди GOP → decoder ждал следующий keyframe (середина: видео замирало до GOP ~2 с; хвост: навсегда). Фикс: `PlayerSession::is_video_track_already_active(track_id)` (capability_selection.rs: selected + active decoder + active requirement + нет pending backend reselection) → `select_video_track` no-op без `VideoTrackSelected` (как audio `AlreadySelected`).
2. **Anchor PTS > target**: HLS `anchor_of_kind_before` выбирает RAP по DTS ≤ target, receipt несёт PTS (у B-кадров +80 мс). `validate_target_and_result` (staged position.rs) больше не отказывает: commit `DecodeForwardToTarget`, floor = target (та же семантика, что обычный seek `landing_policy_for_result`).
3. **Stale prefetch после worker seek**: staged preflight кладёт packets стартовой позиции в `PreparedMedia.prefetch_state`; worker-receipted seek (не `seek_detached`) их не чистил → replay позиции 0 после seek. Фикс: `PreparedMedia::discard_prefetched_events_after_worker_seek()` на authoritative receipt в `service_staged_position_preparation`.
4. **D52 intent `StartPlaying` старому instance-у в EOF drain** → `Play` → replay-seek в 0 → «old playback position moved backward». Фикс: `EofDrainRuntime.continues_started_playback` (`record_drain_origin(previous_state)` в `enter_eof_drain`, Playing/Buffering) → `start_playing_intent_already_in_progress` true. Публичный `PlayerCommand::Play` в drain по-прежнему replay.
5. **Конец media**: (a) часы хвоста экстраполируют за duration (20.0107 с у 20 с) → «candidate is shorter than target» — `old_playback_target_position()` клампит target к public duration (Static); текст ошибки теперь с target/candidate. (b) seek ровно в конец с Play висел в `WaitingForAudioPreroll` — `seek_audio_gate_status` даёт Ready при `WaitingForPreroll` + `seek_audio_tail_exhausted_at_eof()` (EOF drain и 0 pending audio); WaitingForDecoder/Output не трогаются. (c) Drain висел на кадре с PTS за duration (20.04 с) при замёрзшем audio clock — `queued_video_frames_unreachable_after_audio_end()` (audio `DrainedOutput`, Static, front frame ≥ media end) не блокирует `Ended`.

## Итоговая семантика
Switch в любом месте → та же позиция, новый тон; у конца → доигрывает и `Ended`; после `Ended` → новая версия Paused в конце (20000).

## Тесты
- `session/tests/staged_media_install/reopen_tail.rs` — production staged path до presented frame: RAP PTS после target; хвост после последнего RAP / последний кадр + drain + intent / overshoot за duration (decoder сконфигурирован один раз P010, в decoder ушёл только post-seek RAP); публичный Play в drain = replay.
- `session/tests/redundant_video_track_selection.rs` (no-op, другая дорожка, без decoder-а, unknown track).
- `session/tests/seek_audio_tail_at_eof.rs`, `session/tests/eof_drain_past_end_frame.rs`.
- `scrub_hold.rs` fixture удлинён (`CONTINUOUS_VIDEO_PACKETS`), чтобы drag не доходил до EOF.

## Живая проверка
`scratchpad/live/fix21-runs.sh <name> <at>` поверх `run-d6.sh` (loopback `master-al.m3u8` на :8765, uinput KEY_B): середина, хвост 18.1 с, конец, после `Ended` — PASS, тон 440→880 Гц.

Связано: `mem:app-egui/web-audio-versions-menu`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:player-core/core`.
