# HostPlanar upload через StagingBelt + polосная параллельная копия (2026-07-03)

> **Superseded notice (2026-07-03):** любые упоминания hover preview, hover predecode, hover budget/reservation, timeline-hover prepare или hover overlay ниже являются историческими и не описывают активный контракт. Актуальные owners и запреты: `mem:core` и `mem:frame-server/core`. Остальная non-hover информация этой memory остаётся действующей.

## Что сделано
`render-wgpu-video::video/host_planar_upload.rs` (`WgpuHostPlanarUploadBackend`):
- `Queue::write_texture` заменён на `wgpu::util::StagingBelt::allocate` + полосная
  параллельная копия plane block-а в mapped chunk (`copy_plane_block_into_staging`,
  `std::thread::scope`, полосы ≥1МиБ, максимум 4 потока) + `copy_buffer_to_texture`.
- GPU-копии всех planes одного кадра батчатся в ОДИН encoder/submit: trait
  `HostPlanarUploadBackend` получил `flush_plane_uploads()` (default no-op), его
  зовёт `upload_host_planar_visible_rows` после цикла planes; wgpu backend там
  делает `belt.finish()` → `queue.submit` → `belt.recall()`.
- Chunk belt-а 16МиБ (весь 4K-кадр). `bytes_per_row` staging выравнивается к
  `COPY_BYTES_PER_ROW_ALIGNMENT` (256) — chroma stride 1920 не выровнен, поэтому
  per-row repack, а не flat memcpy.

## Почему так
- Одиночный memcpy 8-12МБ внутри write_texture на memory-bandwidth-bound CPU под
  нагрузкой dav1d стоил p50 ~4мс / p99 ~20мс / max 30мс в `video_prepare`.
- ВАЖНО: `create_buffer(mapped_at_creation)` на каждый кадр — АНТИПАТТЕРН: wgpu
  zero-инициализирует буфер, это дороже самой копии (замерено: 25% кадров сверх
  бюджета против 7.8% у write_texture). Только belt/reuse.
- Это НЕ upload-ahead (см. `mem:video-ffmpeg/software-upload-ahead-REJECTED`):
  копия и submit остаются на render-потоке синхронно, отклонённый worker-scheduler
  не возвращён. Belt-подход с явным submit НЕ конфликтует с тем вердиктом.
- `WriteOnly<[u8]>` в wgpu 29 не Send (Sized-bound) — обёртка `StagingCopyBand`
  c `unsafe impl Send` (полосы дизъюнктны через `split_at`); в замыкании нужен
  захват ВСЕЙ обёртки одним place-выражением (edition-2021 precise capture поля
  .0 обошёл бы Send impl).

## Потоки software-декода (та же сессия)
- `video.sw_decode_threads` (config, default 0=auto, 0..=64, живая через
  PlayerDecoderThreadConfig group): 0 → `SoftwareDecodeThreadBudget::Auto`.
- Auto резолвится в `video-core::SoftwareDecodeThreadBudget::resolved_thread_count()`
  = `max(2, available_parallelism − 2)`; те же цифры видит hover accounting
  (`playback_thread_budget_from_decoder_config`) и ffmpeg
  (`ffmpeg_thread_count_from_budget` больше НЕ отдаёт 0=все ядра).
- Причина: 8 dav1d-воркеров на 8 HT (4 ядра KBL) вытесняли render-поток.

## Замеры (AV1 4K60, UHD620 ноутбук, pool=8, панель телеметрии включена)
- До: 7.8% UI-кадров сверх 16.7мс, p99 26.8мс, max 169мс.
- Belt + threads auto(6): 1.7% сверх бюджета, p99 19.8.
- Belt + threads 5 (у пользователя в конфиге): 0.6% сверх бюджета, p99 13.8мс,
  max 40мс, 1 дроп/42с — стабильные 60fps.
- `sw_decoder_surface_pool_frames = 4` (было у пользователя) — катастрофа на ЛЮБОЙ
  версии кода (~130-146 Late-дропов/40с): рендер пинит 2-3 кадра, декодеру не
  остаётся opережения. Минимум для 4K60 — дефолтные 8.

## Замерочный harness
`/tmp/rusti-perf-test/measure.sh` + `analyze.py` (в /tmp, эфемерные): 30с прогон с
`RUST_LOG=fastiplayer::render_frame_timing=trace`, XDG_CONFIG_HOME-подмена конфига,
per-stage перцентили и CPU по потокам из /proc. Логи испорчены ANSI-кодами — grep
только после `sed 's/\x1b\[[0-9;]*m//g'`. Не запускать замер параллельно с cargo.
