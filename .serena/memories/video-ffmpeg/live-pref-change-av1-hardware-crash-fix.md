# Live смена preference software→auto на AV1 роняла плейбек (2026-06-19)

Багфикс: при живой смене `video.preferred_backend` software→auto во время
воспроизведения AV1 HDR плейбек вставал с `RuntimeError: Video decoder thread
stopped before accepting packet: Decoder thread disconnected`. Дополняет
`mem:render-video/live-backend-swap-present-frame-freeze` и
`mem:video-ffmpeg/av1-mp4-keyframe-decode-fix`.

## Корневая причина
Live-применение runtime-настроек идёт через `app-egui`
`frame_prepare.rs :: FrameSettingsRuntimeAdapter::apply_player_runtime_settings`
(НЕ через reactive `apply_video_backend_reselection`). Оно вызывало
`rebuild_video_pipeline_with_decoder_config(..., stream_requirement = None, ...)`.
В `video_pipeline_selector` `output_serves_requirement(output, None)` =
`is_none_or` → true (фильтр выключен), поэтому `auto` брал ПЕРВЫЙ playable VAAPI
dma_buf output и возвращал `VaapiDmaBufWgpu` независимо от того, что реально играет
AV1. VAAPI не умеет AV1 → новый decoder thread при configure стрима падает →
канал к нему disconnected → player-core при отправке пакета даёт fatal
"Decoder thread disconnected".

Reactive путь (`apply_video_backend_reselection`) этим НЕ страдал: он получает
requirement из события `VideoBackendSelectionRequested` и передаёт его в селектор,
плюс короткозамыкает при совпадении backend kind. Баг был только в live-settings
пути, который терял requirement.

## Фикс (app-egui, 2 файла + 1 guard-тест)
- `state.rs`: новое поле `AppState.active_video_stream_requirement:
  Option<VideoDecodeRequirement>` + boundary getter
  `active_video_stream_requirement(&self) -> Option<&VideoDecodeRequirement>`.
  `note_video_backend_reselection_request` теперь кэширует
  `request.requirement.clone()` в это поле (событие эмитится на КАЖДОЙ активации
  трека из `capability_selection.rs::select_default/requested_video_track` →
  `note_active_video_stream_requirement`, в т.ч. когда software сам тянет AV1).
- `frame_prepare.rs`: live `apply_player_runtime_settings` клонирует
  `active_video_stream_requirement()` ДО `&mut`-вызова rebuild и передаёт его
  вместо `None`. Теперь `auto` для software-only кодека (AV1) корректно остаётся
  на FFmpeg software; для hw-decodable стрима по-прежнему берёт hardware. Это
  чинит и live-смену decoder_thread_config (sw frame pool) на auto+AV1, которая
  падала тем же путём.

## Не задето
- Селектор `select_video_pipeline_plan`/`output_serves_requirement` не менялись
  (None по-прежнему = фильтр выключен; это корректно для случаев без активного
  стрима, например первый init pipeline).
- Reactive reselection, backend swap freeze, AV1 keyframe re-seek — без изменений.

## Известное ограничение (осталось)
Live-смена preference, дающая ТОТ ЖЕ backend kind (auto+AV1 → снова ffmpeg),
всё равно пересоздаёт decoder thread (rebuild не короткозамыкает по неизменному
плану, т.к. тот же путь обслуживает реальные смены decoder_thread_config). Это не
крэш, но возможен короткий re-seek-хиккап. Бесшовный skip при идентичном плане —
отдельная оптимизация, не делалась.

## Проверено
app-egui 150 тестов (новый guard
`live_settings_rebuild_passes_active_stream_requirement`), fmt clean, guardrails OK,
build OK.
