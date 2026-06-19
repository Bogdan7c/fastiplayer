# Upload-ahead для FFmpeg software HostPlanar — ОТКЛОНЕНО, НЕ ПРЕДЛАГАТЬ ПОВТОРНО

ВЕРДИКТ (2026-06-19): фича «upload-ahead» (вынос host→GPU upload software-кадров на отдельный
worker-поток с renderer-owned scheduler-ом) НЕ РАБОТАЕТ и отклонена пользователем. Весь код
реверчен: `main` и origin/main приведены к коммиту `93fe41f` "дополнено". Сессии 1–4
(`Player-core upload-ahead lease boundary`, `Render-wgpu-video HostPlanarUploadScheduler`,
`Config/Settings UI для software_upload_ahead_frames`, `App-egui wiring/hotswap/fallback`) удалены.

НЕ предлагать это снова — ни ассистенту, ни пользователю. Если возникает идея «давайте грузить
кадры заранее в фоновом потоке, чтобы разгрузить render» для software/FFmpeg HostPlanar пути —
СТОП, это уже пробовали, см. ниже почему не взлетает.

## Что пробовали
- `player-core` выдавал renderer-neutral upload-ahead leases (current + queued frames).
- `render-wgpu-video::HostPlanarUploadScheduler`: worker-поток, bounded queue, один shared
  `HostPlanarUploadTexturePool`, ready-map, epoch/clear, диагностика.
- `app-egui` включал scheduler только для `FfmpegHostUploadWgpu` при
  `video.software_upload_ahead_frames > 0` (default 3), depth=0 = старый синхронный materializer.
- Идея: убрать host→GPU upload из render hot path, чтобы `video_prepare` был дешёвым.

## Почему не работает (корневая, аппаратно-архитектурная причина)
Целевое железо — iGPU c общей памятью и ОДНОЙ WGPU-очередью. На wgpu `Queue::write_texture` НЕ
делает GPU-копию сразу: он только складывает данные в per-device staging (CPU memcpy на стороне
вызывающего потока), а фактическая GPU-копия флашится на СЛЕДУЮЩЕМ `Queue::submit` — а submit
делает только render-поток. Плюс wgpu сериализует write_texture/submit через внутренний device-mutex
(подтверждено по context7). Итог: worker НЕ может реально грузить в GPU параллельно с рендером.
Максимум, что фича даёт — переносит CPU-staging memcpy (~2.3 мс/кадр на 4K) с render-потока на
worker, но эта стоимость в основном возвращается в `renderer_submit` (0.68 → 2.4 мс), т.к. GPU-копия
всё равно флашится в render submit и ждёт device-lock.

## Замеренный результат (release smoke, 4K60 VP9, software, depth=3 vs depth=0)
- sync (depth=0): frame_total avg 3.36 мс, p95 5.03; video_prepare 2.30; renderer_submit 0.68.
- upload-ahead (исправленный): frame_total avg 3.08 мс, p95 4.27; video_prepare 0.02;
  renderer_submit 2.38. Чистый выигрыш по frame_total ~8% — маргинальный, в пределах «и так ок».
- ПЕРВАЯ реализация к тому же РЕГРЕССИРОВАЛА плейбек: worker держал texture-pool mutex весь
  `write_texture`, render-поток не мог взять try_lock ~97.5% кадров → переиспользование прошлого
  кадра почти каждый кадр → «дичайшие тормоза» (judder). Даже после фикса (write_texture вне
  pool lock + ready-map-first lookup) чистая выгода осталась маргинальной.

## Оценка пользователя
Фича не выполнила свою функцию; субъективно плейбек был ЛУЧШЕ без неё. Сложность (worker, scheduler,
pool concurrency, hotswap, lease boundary в player-core) не оправдана ради ~8% и запаса под нагрузкой.

## Если host→GPU upload когда-нибудь станет реальным узким местом
НЕ возвращать worker+`write_texture` scheduler на этом iGPU/single-queue стеке. Реальный потолок есть
только при смене вводных: дискретная GPU с выделенной transfer-очередью, либо принципиально другой
механизм загрузки. Сам этот подход — тупик на текущем железе, проверено.
