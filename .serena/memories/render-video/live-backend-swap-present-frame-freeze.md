# Живая смена video backend во время плейбека: freeze present-кадра (Session 26)

Багфикс к фиче «смена backend/render на лету»: ручной свап hardware->software во
время активного H265 (vaapi P010 DMA-BUF) падал
`UnsupportedRenderFormat: Render resources are missing for P010 frame handle N in
generation 1`. Дополняет `mem:video-ffmpeg/sw-to-hw-backend-swap-frame-contract-fix`.

## Корневая причина (гонка жизненного цикла)
1. `pipeline.set_video_decoder_thread_handle` НЕ освобождал удержанный present-кадр
   и НЕ двигал render_generation.
2. reseek после свапа чистит только очередь будущих кадров (`clear_queued_video_frames`),
   но НАМЕРЕННО сохраняет текущий кадр на экране; `render_generation` бампается
   ТОЛЬКО при media reset (`media_lifecycle.rs`), seek двигает отдельный
   `seek_generation`, не render.
3. `PlayerWorker::set_video_backend` — асинхронный `try_send`, а `wgpu_frame_materializer`
   в app свапается синхронно в `rebuild_video_pipeline_with_decoder_config`.
   => В окне между подменой materializer-а (HostPlanar) и обработкой команды worker-ом
   app отдаёт старый P010-кадр vaapi (тот же render_generation) в новый materializer ->
   handle не найден -> `Missing` -> fatal. Авто-свап AV1 HW->SW этого не ловил, т.к.
   там свап во время deferral (present-кадры ещё не текут).

## Фикс (player-core + app-egui, поведение в окне = «заморозить последний кадр»)
- player-core `PlayerSession::set_video_backend` (session.rs): ПЕРЕД установкой нового
  decoder handle, если `pipeline.has_active_video_decoder()`, вызывает
  `clear_video_frames()` (release кадров старого backend-а через ЕГО provider) +
  `advance_render_generation()`. Так кадры/lease нового backend-а получают свежее
  поколение, а старые отсекаются штатной generation-проверкой. Первый install при
  старте (нет активного decoder-а) bump не делает. Тест:
  `swapping_active_backend_advances_render_generation_but_first_install_does_not`.
- app-egui `AppState` (state.rs): поля `backend_swap_frozen_frame:
  Option<CachedRenderablePresentFrame>` + `backend_swap_from_generation: Option<u64>`.
  В `rebuild_video_pipeline_with_decoder_config` при РЕАЛЬНОЙ смене класса backend-а
  (`previous_backend_kind != plan_backend_kind`) -> `begin_backend_swap_video_freeze()`
  фиксирует render_generation момента свапа и копию последнего материализованного кадра
  (его wgpu texture views живы через Arc даже после дропа старого materializer-а).
  `backend_swap_video_phase(snapshot)`: пока `render_generation == from_generation`
  (worker не переключился) ИЛИ переключился, но `current_video_frame` ещё None (новый
  decoder не выдал кадр) -> `HoldFrozenFrame` (рендерим замороженный кадр, НЕ
  материализуем кадры старого backend-а). Когда worker выдал первый кадр нового
  backend-а (gen продвинулся И current_video_frame.is_some()) или сменился источник ->
  `finish_backend_swap_video_freeze` -> обычный путь. `prepare_video_frame`
  (frame_prepare.rs) проверяет фазу ДО acquire и при HoldFrozenFrame возвращает frozen
  кадр (или empty, если кэша не было) без lookup/ошибки.

## Не закрыто / проверить вживую (нет GPU-теста у агента)
- Валидность wgpu TextureView замороженного кадра после дропа старого DmaBuf
  materializer-а (архитектурно ок: view держит texture через Arc) — подтвердить на
  железе Intel UHD 620.
- Кросс-backend release lease старого provider при finish (provider жив через lease
  handle Arc).

## Проверено
player-core 369 тестов, app-egui 149, guardrails OK, fmt clean. Session 25 AV1
(SDR+HDR+software) и contract-recompute тест не регрессировали.


## S25 same-item source switch (2026-07-22)
- Same-item exact Installed reuses this frozen-frame lifecycle before candidate pointer commit for both same-class and cross-class backend changes. The old frame remains Arc-owned until the new render generation presents a frame.
- A generation switch to an audio-only source is also a terminal freeze condition; it releases the old frozen frame instead of waiting forever for an impossible video frame. Full orchestration: `mem:app-egui/same-item-candidate-switch-s25-2026-07-22`.
