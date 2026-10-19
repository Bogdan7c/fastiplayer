# CPU/RAM/GPU-паритет с VLC (issue #10) — состояние плана

План и решения владельца: `user/cpu-parity/README.md`; отчёты сессий — `user/cpu-parity/results/NN.md`. Начато 2026-10-19 (сессия 00 — диагностика, код плеера не менялся).

## Решения владельца (2026-10-19)
- Сначала аппаратный путь (сессии 01–06), потом программный (07–08). LTO + `codegen-units = 1` — только бонус в конце (09), чтобы не смешивать эффекты. Эталон — обычный VLC с Qt-интерфейсом (не `--intf dummy`).
- Запреты issue #10 в силе: без CPU readback/конверсии HW-кадров, без роста очередей/пулов/кэшей, без систематического роста памяти без решения владельца, без альтернативного рендерера; upload-ahead для SW отклонён (`mem:video-ffmpeg/software-upload-ahead-REJECTED`).

## База (780M, 2026-10-19, % одного ядра, окно 20 с; VLC с Qt UI)
HW H.264 1080p60: 8,3 (главный поток 5,1 / worker 1,1 / декодер 1,5) vs VLC 2,9. HW HEVC 4K60: 12,3 vs 4,2. HW H.264 4K30: 7,6 vs 1,6. SW H.264 1080p60: 30,3 vs 25,3 (декод-потоки равны, 21,7 vs 21,6). SW AV1 4K60: 178 vs 203. RSS у нас ниже везде; GPU gfx у нас выше в 1,5–2,5 раза.

## Корневые причины (подтверждены)
1. Нет настоящего пула VA-поверхностей: `vaCreateSurfaces`/`vaDestroySurfaces` на каждый кадр (`video-vaapi/src/frame_pool.rs` хранит Copy-дескрипторы; поверхность создаёт `InternalVaapiFrame::to_native_handle` в `VaapiPicture::new`; `alloc_or_allocate` не исчерпывается). Счётчики: `imports_created` 60/с, `imports_reused=0` (контракт `explicit_external_memory_reuse_sync: false`), `imports_replaced≈created`. → сессия 01.
2. Vulkan-импорт DMA-BUF на каждый кадр: `DmaBufWgpuTextureCache` ключуется по `FrameResourceHandle`, уникальному на кадр (`video_prepare` 0,25 мс 1080p → 0,72 мс 4K при zero-copy). → сессия 03 (кэш по surface id + inode + явный барьер VA→Vulkan).
3. egui-проход + present на каждый vsync независимо от fps источника (`redraw_pacing.rs`, `frame_prepare.rs`). → сессия 04 (рендер по событию/dirty UI-модели).
4. Холостые пробуждения каждые 2 мс: декодер при полном канале кадров `recv_timeout(2 мс)` и пульсирует worker на каждом повторе (`runtime_loop.rs`, `frame_publication.rs`), worker опрашивает `DecodeReadiness` каждые 2 мс (`wakeup.rs`, `worker.rs:91`); `pauses≈1500/с`. → сессия 02.
5. Россыпь: 2 клона `PlayerSnapshot` на vsync + пересборка на тик, 3 клона MPRIS-снимка, dup fd до проверки кэша, bind group на кадр, `thread::scope` на каждую SW-загрузку кадра. → сессии 05, 07.
6. `[profile.release]` отсутствует. → сессия 09.

## Инструменты
`docs/benchmarks/tools/dev/{thread_cpu.py,run_pass.py,agg_timing.py}` — быстрый срез по потокам/стадиям/GPU (не протокол). Строгий протокол PR #12 (`docs/benchmarks/tools/local/`) в истории `main` отсутствует (переписана при cutover) — восстанавливать из осиротевшего merge-коммита `09ab9060` (сессия 06). samply установлен в `~/.cargo/bin`, требует `perf_event_paranoid ≤ 1`. Фикстуры — `benchmark-fixtures/` (gitignore), генератор `tools/t480s/make_fixtures.py`.

## Открытые вопросы качества
- Реальный 60 fps H.264: 809–898 уникальных кадров из 900 handoff-ов (PR #12) — проверить после сессий 02/04.
- Равенство доставленных кадров в SW AV1 против VLC не доказано.
