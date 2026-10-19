# Сессия 07: программный путь — загрузка кадров без лишней работы

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 07 плана `user/cpu-parity/`: убери создание потоков на каждую загрузку кадра и лишние submit-ы в host-planar пути FFmpeg → wgpu, не нарушая запреты issue #10 и отклонённый upload-ahead. Требует сессии 04. Сначала прочитай `user/cpu-parity/README.md`, `results/00.md`, `results/04.md`, `mem:video-ffmpeg/software-upload-ahead-REJECTED`, `mem:render-video/host-planar-staging-belt-upload`, `mem:video-ffmpeg/software-frame-pool-setting`.

## Что найдено

- `crates/render-wgpu-video/src/video/host_planar_upload/wgpu_backend.rs:41-170, 225`: на каждый новый кадр — memcpy всех плоскостей в `StagingBelt` (16 МиБ чанки), `copy_buffer_to_texture` в общий encoder, отдельный `queue.submit` + `staging_belt.finish()/recall()`; при плоскости ≥ 1 МиБ `std::thread::scope` создаёт до 3 дополнительных OS-потоков **на каждый кадр** (`HOST_PLANAR_STAGING_COPY_MAX_THREADS = 4`). Создание/уничтожение потоков 60 раз в секунду — чистая потеря и шум для планировщика.
- `host_yuv420_renderer.rs:272, 336`: bind group и uniform на кадр (общая правка с сессией 05 — согласовать, кто делает).
- Главный поток в SW 1080p60 — 6,6 % против 5,1 % в HW: разница ≈ стоимость staging-копии 1080p (~3 МиБ) на кадр.

## Архитектура

- **Владелец:** `render-wgpu-video::host_planar_upload` (бэкенд wgpu). Решение по потокам: либо один постоянный маленький пул помощников для memcpy (живёт столько же, сколько материализатор; задача — только копирование в уже выделенный staging, без wgpu-вызовов — это **не** upload-ahead и не scheduler), либо без помощников вовсе, если замер покажет, что однопоточный memcpy 1080p укладывается с запасом (на 4K — проверить отдельно). Выбор — по замеру `video_prepare` p90, а не по вкусу.
- **Один submit на кадр:** загрузка плоскостей должна попадать в тот же `queue.submit`, что и рендер кадра (encoder кадра), если порядок boundary egui-wgpu это позволяет; `StagingBelt::recall` — после submit кадра.
- **Memcpy не удваивать:** если `Queue::write_texture` на данном стеке делает ту же копию в staging, но без второго прохода, сравнить честно; оставить вариант с меньшим суммарным CPU при равной плавности (`mem:video-ffmpeg/software-upload-ahead-REJECTED` объясняет, почему wgpu `write_texture` — это тоже CPU-копия).
- **Запреты:** CPU-конверсия/RGB, очередь загрузки наперёд, рост пула (`sw_decoder_surface_pool_frames` остаётся 8 по умолчанию).

## Инварианты

1. Один кадр — одна копия host → GPU (решение 2026-06-15), никаких промежуточных копий.
2. Пул текстур `HostPlanarUploadTexturePool` ограничен 24 слотами, `Busy` при полном — как сегодня.
3. Release кадра декодеру — только после `on_submitted_work_done`.
4. Кадры 10/12-бит и 4:2:2/4:4:4 — та же матрица v1, без регрессий.

## Тесты

- Fake-бэкенд считает создание потоков/вызовы копирования: серия кадров → 0 новых потоков после старта; падает на текущем коде.
- Тест «один submit на кадр» на fake queue.
- Существующие тесты host-planar (контракт, Busy, eviction) зелёные.

## Замер и критерий выхода

- До/после `run_pass.py` SW H.264 1080p60, SW AV1 4K60; `agg_timing.py` `video_prepare` mean/p90 не хуже, число потоков процесса стабильно (без всплесков в `ls /proc/PID/task`).
- Quality-gate README на SW-файлах, включая 4K60 AV1 (нет роста `drops_*`, FPS держится).
- Отчёт `results/07.md`, обновить `mem:render-video/host-planar-staging-belt-upload`.
