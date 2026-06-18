# SW->HW backend swap: stale frame contract fix (2026-06-18, Session 26)

Исправлен баг: при смене ролика software-клип (AV1 10-bit HDR, ffmpeg-sw,
host-upload YUV420P10LE) -> HW-decodable ролик (HEVC/VP9 10-bit P010) видео
падало с `UnsupportedRenderFormat: decoder backend does not support frame contract
... software host upload` и не стартовало через hardware.

## Корневая причина
При открытии нового ролика, пока активен ffmpeg-sw backend,
`select_default_video_track` -> `validate_video_decode_requirement` находит
playable output активного (software) backend-а, и трек активируется с SOFTWARE
host-upload контрактом (новый ролик, но контракт под ffmpeg-sw). Свапа в
player-core НЕ происходит (ffmpeg software сам декодит HEVC/VP9), поэтому
`pending_video_backend_reselection` пуст. Затем `app-egui` (auto preference)
видит HW-decodable стрим и через `select_video_pipeline_plan` свапает на vaapi ->
`set_video_backend`. В ветке БЕЗ pending reselection вызывался
`configure_active_video_decoder_stream`, который РЕЮЗАЛ
`pipeline.active_video_frame_contract()` (software host-upload) и отдавал его
vaapi-backend-у -> `UnsupportedFrameContract`.

## Фикс (1 файл + 1 тест)
`crates/player-core/src/session/capability_selection.rs` ::
`configure_active_video_decoder_stream` теперь:
1. Берёт active track + active requirement.
2. ЗАНОВО валидирует requirement через `validate_video_decode_requirement`
   (active_video_backend_id уже = новый backend) и из matched output берёт
   актуальный `frame_contract` (или `fallback_frame_contract_for_unprobed_requirement`),
   а не реюзает `active_video_frame_contract()`.
3. Конфигурит decoder + `set_active_video_selection(requirement, frame_contract)`.
4. Вызывает `reseek_to_current_position_after_backend_swap()` — новый decoder
   стартует с пустого DPB и обязан получить KEY_FRAME (симметрично pending-пути
   `retry_pending_video_backend_reselection`); иначе видео ждёт следующего keyframe.

Тест: `switching_to_hardware_backend_recomputes_active_frame_contract` в
`session/tests/capability_selection.rs` (ffmpeg-sw выбирает VP9 с
host_yuv420_planar8 -> set_video_backend(vaapi) -> configured stream на vaapi
имеет dma_buf_nv12, трек остаётся выбранным).

## Не задето / не сломано
- AV1 keyframe probe + re-seek (`mem:video-ffmpeg/av1-mp4-keyframe-decode-fix`,
  HW->SW pending-reselection путь) — отдельная ветка, не менялась.
- `select_video_pipeline_plan` stream-aware selection (app-egui) не менялась.
- player-core 369 tests, codec-core OK, guardrails OK, fmt clean, app-egui build OK.
