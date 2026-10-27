# GLES2: программный кадр без CPU-копии (сессия 12 плана gles2-renderer, 2026-10-27)

Отчёт: `user/gles2-renderer/results/12.md`. Решения владельца 59–61 README: реализовано и принято по короткому замеру; вид памяти блоков выбирает приложение по семейству рендера; сброс кэша CPU решает самопроверка; только 8 бит; fail-closed на копию. Код — `c82088a8`.

## Цепочка
FFmpeg (`get_buffer2` 08b) пишет кадр в блок ЗАПЕЧАТАННОГО memfd → рендер GLES2 делает из блока dma-buf (`/dev/udmabuf`, один на блок) → три `EGLImage` R8 (Y/U/V, текстура ровно размера плоскости, шаг — pitch EGL) → та же планарная программа 8 бит, что у копии.

## Владельцы и API
- `video-core::resource::host_import`: `HostFrameMemoryKind::{Anonymous (default), SealedMemfd}`, `SealedMemfdRegion{memfd, offset}`, `HostImportableMemoryBlock::sealed_memfd()` (cfg linux, default None). Контракт: память анонимная ИЛИ `MAP_SHARED` memfd с `F_SEAL_SHRINK` без `F_SEAL_WRITE`.
- `video-ffmpeg`: `ffi/host_frame_memory/sealed_memfd.rs` (memfd_create → ftruncate → SHRINK|GROW|SEAL → mmap), `HostFrameBlockPool::new(kind)`, `SoftwareFrameMemory::ImportableBlocks{memory}`, `FfmpegDecoderThreadConfig` в `decoder_thread/config.rs` (+`with_host_frame_memory`), `FfmpegSoftwareVideoBackendFactory::with_host_frame_memory`. Отказ memfd → fallback FFmpeg + счётчик.
- `app-egui::video_pipeline_candidate::resource_driver::host_frame_memory_for(RendererFamily)`: Gles2 → SealedMemfd, WgpuVulkan → Anonymous (amdgpu userptr wgpu-пути 08b принимает только анонимную память — НЕ менять).
- `gles2-video` (без новых deps): `DmaBufPlanarFrame`/`DmaBufPlanarIdentity{resource_id, object, layout, luma_extent, planes:[DmaBufPlaneLocation{offset,pitch};3]}`, `VideoLayerRenderer::{show_cached_planar_dma_buf_frame, import_planar_dma_buf_frame, planar_dma_buf_import_snapshot}`; кэш `DmaBufImportCache<T, K>` обобщён по ключу; текущий кадр ровно один: кольцо | NV12-импорт | планарный импорт; рисует `PlannedFrame::Planar`. Самопроверка `probe_cpu_write_visibility(context, &mut dyn CpuWriteProbeProducer) -> CpuWriteVisibility::{CoherentWithoutCacheFlush, VisibleAfterCacheFlush, NotVisible}` (буфер ≥ `CPU_WRITE_PROBE_MIN_BUFFER_BYTES` = 16384, свой FBO, состояние GL восстанавливается, вызывать вне кадра).
- `render-gles2-shell::host_block_import` (владелец пути): `HostBlockZeroCopy::{try_show → ZeroCopyShow::{Shown, UseUpload}, note_frame_uploaded, decoder_changed(BlockDmaBufCapacity), log_summary, stats}`; доступность лениво на первом подходящем кадре: уровень zero-copy драйвера → `/dev/udmabuf` → самопроверка на своём udmabuf (`probe_buffer.rs`) → `CpuCacheFlushPolicy::{NotNeeded, FlushVisibleRows}` или `SoftwareZeroCopyDisabled`; dma-buf блоков — LRU `block_dma_bufs.rs` (ёмкость = пул плана, purge при смене декодера); `CLFLUSH` видимых строк каждого нового кадра (без аллокаций). `unsafe` только в `host_block_import/kernel.rs` (crate `#![deny(unsafe_code)]`). Ошибка импорта → путь выключен до пересоздания рендера. Публично: `Gles2Renderer::software_zero_copy_stats() -> SoftwareZeroCopyStats`. Вызов — `video_import::layer_feed::show_host_planar` (до копии).

## Живые факты
- crocus (T410) и radeonsi принимают линейные R8 из udmabuf с шагом 2304/1280; проход GPU тот же, что из загруженной текстуры; скрытой копии нет. Импорт на кадр +0,7 мс system — нужен кэш на блок.
- Ironlake без LLC: без `CLFLUSH` GPU видит старые данные (стенд 231/240). Самопроверка на T410 → `FlushVisibleRows`; на 780M/llvmpipe → `NotNeeded`. Сброс у настоящего декодера 1,2–1,7 мс/кадр 1080p (кадр к показу почти вытеснен из кэша), синтетически «горячий» — 4,7 мс.
- Итог T410 (короткий замер на батарее, база → новый): SW H.264 1080p60 CPU процесса ~125 → ~118 % (Wayland), ~127 → ~119 % (XWayland), главный поток ~22 → ~16 %; HEVC 1080p30 ~125 → ~121 %; кадры и Render/3D те же.
- Права: `/dev/udmabuf` — ACL пользователя от сессии рабочего стола (CachyOS, systemd uaccess) на обеих машинах; лимит модуля на T410 64 МиБ.

## Тесты
- `render-gles2-shell`: `src/host_block_import/tests.rs` (12, вкл. kernel), `tests/ffmpeg_decoder_to_shell.rs::ffmpeg_vp9_frames_in_memfd_blocks_reach_the_screen_without_a_cpu_copy`; тестовые бинари гонялись на T410 (fixture по абсолютному пути сборки). Мутация «без сброса» падает на T410.
- `gles2-video`: `tests/dma_buf_planar.rs`, `tests/dma_buf_planar_cache.rs`, `tests/cpu_write_probe.rs`.
- `video-ffmpeg`: memfd-пул, `direct_frame_buffers/tests.rs` (memfd побайтно = default), `decoder_thread/tests/host_frame_memory_kind.rs`.
- Стенд: `gles2-egui/examples/udmabuf_zero_copy_probe.rs` (`--mode upload|import-cached|import-per-frame --verify --no-flush --flush-range --measure-reflush`).

## Ограничения / дальше (12a)
Linux x86_64 + доступный udmabuf; 8 бит (10/12 и AV1 — копия); `CLFLUSH` в главном потоке — кандидат 12a; seek/пауза/смена файла и счётчик `dma_bufs_created` при ёмкости 24 — в переквалификации 12a.
