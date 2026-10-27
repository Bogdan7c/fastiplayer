# Подсессия 12a.1: «почти тянущий» HW на T410 (2026-10-27/28, DONE)

Отчёт: `user/gles2-renderer/results/12a.1.md`, решения владельца 73–76 README плана. Следующая: 12a.2 (`user/gles2-renderer/12a.2-decode-present-phase.md`).

## Корень
- На Ironlake H.264 VA-API и проход показа GLES2 делят одну FIFO-очередь GPU без вытеснения. Декод картинки: 720p ~10,7 мс GPU, 1080p ~27 мс. Разбор и отправка — ~4 мс CPU (`submit_ms`).
- В `NonBlocking` (09a) ждать GPU умела только очередь «в полёте» video-vaapi, которая видит лишь кадры `FrameReady`. Картинки, которые H.264 держит для перестановки B-кадров, не учитывались. После паузы (ключевой кадр) в GPU уходила пачка из 5–7 картинок, и проход показа опаздывал к vsync. Пропуски шли кучками раз в 2 с.
- Решение 73: патч cros-codecs, `BlockingMode::SyncPreviousPicture` (`src/lib.rs`). Политика `src/submitted_picture_sync.rs` (`SubmittedPictureSync`, трейт `AwaitablePicture`) собрана без фичи `backend`, поэтому тесты идут в `cargo test --manifest-path crates/cros-codecs-patch/Cargo.toml`. Тесты декодеров cros-codecs 0.0.6 с `backend` не компилируются ещё в upstream (dummy backend устарел). `StatelessDecoder.submitted_picture_sync` зовётся после `submit_picture` во всех 5 кодеках, `flush` дожидается запомненной картинки. video-vaapi `codec_adapter::STATELESS_DECODER_BLOCKING_MODE = SyncPreviousPicture`.
- Инвариант: после каждой отправки в ускорителе не больше двух недожданных картинок.

## Числа T410 (ABBA, Wayland, фуллскрин)
- 720p60: пропуски vsync 19–27 → 1 за 20 с, 60,1 кадра/с.
- 1080p30: 30,1 кадра/с, опоздавших кадров (интервал > 50 мс) 92–98 → 31–33 — это остаток для 12a.2.
- CPU: +1 п.п. на 720p (поток `video-decode` 5,8 → 6,9 % на 720p30), на 1080p30 −0,8. Решение 75: цена принята.
- 780M: пропускная способность VA-API без изменений (4K60 ~370–379 кадров/с).

## Методы и грабли
- Пропуск виден в трассе `render_frame_timing`: `surface_present_ms` 33 вместо 16. Отправки декода — TRACE `video_vaapi::decoder` «decode() called». Скрипты (`frame_gaps.py`, `submit_phase.py`, `cpu_ab.sh` со `schedstat`) — в scratchpad сессии.
- Порог пропуска — 1,5 номинального интервала. На 30 fps протокол v4 опоздания не видит, только непоказанные кадры.
- Профиль ядра на T410 требует `sudo sysctl kernel.perf_event_paranoid=1 kernel.kptr_restrict=0` (сбрасывается при перезагрузке). Строки ядра нашли путь `drm_clflush_sg` ← i915 ← `drm_intel_gem_bo_map` (libdrm `intel_bufmgr_gem.c:1461`) ← i965. Отладочных символов i965 на debuginfod нет; libdrm символизируется через gdb + `DEBUGINFOD_URLS=https://debuginfod.archlinux.org`.
- Сброс кэша i965 — ~26 % потока декода ещё до правки (кандидат 12a.2).
- Аппаратный тест паузы (`h264-mp4-vaapi-paused-frame`) после правки проверяет точный инвариант: «каждый неготовый кадр доделан правилом простоя». Правило простоя закрыто юнит-тестом очереди `decoder_idle_hands_out_every_unfinished_frame_in_order_and_counts_it_as_idle`.
- Бэклог п. 17 («пул 16 дёргает интерфейс»), вероятно, объясняется неспрятанной панелью: на пуле 24 тоже бывает 1001 кадр `ui_model_changed`.
