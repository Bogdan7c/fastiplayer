# Громкость и mute между запусками (UX edge cases, сессия 11, 2026-10-06)

## Решения владельца
- Хранить в основном `config.toml`, без отдельного state-файла. `audio.volume` теперь означает «Громкость» = последняя слышимая громкость (раньше «по умолчанию»). Новый `audio.muted: bool` (default false, additive, schema остаётся v11) — mute отдельно от громкости.
- Запись: через 3 с после последнего изменения (`USER_AUDIO_LEVEL_PERSIST_DEBOUNCE`) + при suspend/exit. При старте побеждает сохранённое.
- Apply в окне настроек с изменённой громкостью/mute сразу меняет текущее воспроизведение.

## Владельцы и boundary
- `app-egui::user_audio_level` — чистый value type `UserAudioLevel { audible_volume: VolumePermille(0..=1000), mute: MuteState }`: `from_audio_config` (volume 0 нормализуется в Muted), `after_player_observation`, `is_confirmed_by`, `player_restore_commands()` (= `SetVolume(слышимая)` затем `SetVolume(0.0)` при mute — порядок обязателен, иначе unmute вернёт fallback), `send_user_audio_level_to_player`.
- `settings_runtime/user_audio_level_persistence.rs` (поле `SettingsRuntime::user_audio_level`) — наблюдение за `PlayerSnapshot.volume/muted` и отложенная запись через одно-полевые `commit_runtime_setting_with_runtime_adapter` (как ширина sidebar; громкость+mute одновременно = 2 commit-а). API: `begin_player_audio_level_restore(now)`, `record_player_audio_observation(obs, now)`, `next_user_audio_level_persist_deadline`, `flush_due_user_audio_level`, `flush_pending_user_audio_level`, `deliver_user_applied_audio_level` (только после user Apply/OK).
- Sync guard `PlayerAudioLevelSync::{NoPlayerBinding, AwaitingConfirmation{expected, give_up_at}, Following}`: пустой snapshot нового worker-а (volume 1.0) и старое состояние до Apply не сохраняются, пока плеер не подтвердит отправленный уровень; таймаут 2 с.
- `SettingsRuntimeReconfigureHost::apply_user_audio_level_to_playback(level) -> PlaybackAudioLevelDelivery::{Sent, NotDelivered}` (default NotDelivered); `FrameSettingsRuntimeAdapter` шлёт команды в player.
- Routing: `audio.muted` в Player route без worker payload (`player_update_from_settings` пропускает), группа `PlayerDefaultVolume`; contract как у `audio.volume`. `audio.volume` по-прежнему обновляет worker `default_volume` (fallback ToggleMute).
- Wiring: `AppShell` после создания AppState шлёт уровень из `begin_player_audio_level_restore` (заменил `desktop_effective_volume`, метод удалён); MPRIS стартует с `user_audio_level().effective_player_volume()`. Покадрово — `frame_prepare/runtime_settings_persistence.rs` (sidebar + громкость); lifecycle — `app_shell/runtime_settings_flush.rs` (`flush_runtime_settings_for_lifecycle_boundary`, свежий snapshot перед flush) через `frame_prepare::with_lifecycle_settings_adapter`. `CommittedConfigSnapshot::default_volume_for_new_media` переименован в `remembered_audible_volume`, добавлен `user_audio_level()`.
- player-core не менялся (громкость «до mute» по-прежнему private `last_nonzero_volume`).

## Ограничения
- Громкость «до mute» app отслеживает сам по snapshot-ам: если громкость сменили и сразу замьютили в пределах одного кадра, сохранится предыдущая слышимая громкость.
- Неудачная запись не ретраится до следующего изменения. В режиме «только в памяти» (сессия 05) каждая отложенная запись показывает ту же плашку, что и sidebar.

## Тесты
- `app-egui/src/user_audio_level/tests.rs`, `settings_runtime/tests/user_audio_level.rs` (рестарт через реальный файл, mute, 100 движений = 1 запись, placeholder snapshot, timeout, битый config → default, Apply → плеер, ошибка commit-а).
- `player-core/src/session/tests/restored_user_volume.rs` (output создан заглушённым, PCM идёт, unmute → сохранённая громкость; fake `ScriptedAudioOutputHandle::applied_volumes`).
- `audio/src/output/processing.rs` tests (масштабирование сэмпла громкостью), `config/src/store/tests/persistence_sections.rs` (v11 без `muted`), `fastiplayer-settings/src/routing_audio_muted_tests.rs`.
