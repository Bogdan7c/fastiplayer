# Квалификация HW против эталона (cpu-parity сессия 06, 2026-10-21) — протокол v2

Отчёт: `user/cpu-parity/results/06.md`. Статус: ОСТАНОВЛЕНА на quality-gate (критерий выхода не выполнен), предложены сессии 06a/06b/06c (см. `user/cpu-parity/README.md`).

## Инструменты (`docs/benchmarks/tools/local/`, README — протокол v1 и таблица diff v2)
- Восстановлены байт-в-байт из осиротевшего merge-коммита PR #12 `09ab9060` (коммит `5c5a5f29`); зависят от `tools/t480s/{collect.py,fullscreen.py}` (render_events, KWin-фуллскрин).
- `collect.py` — одна попытка (transient user systemd unit + cgroup tree CPU, RSS/PSS, DRM fdinfo, доказательства плеера); `PLAYER_COMMANDS` = vlc (v1 история) | mpv (эталон v2); `PROTOCOL_VERSION = 2`.
- `evidence.py` — Fastiplayer: строка `VA-API codec adapter configured for stream backend_name="VA-API <codec>"` + `VA-API Vulkan DMA-BUF` + нет `ffmpeg-host-upload-wgpu`, MPRIS + inode, render handoff identities (diagnostic); mpv: JSON IPC (`mpv_properties`, ответы по `request_id`), `hwdec-current ∈ {vaapi, vulkan}` на обеих границах, `time-pos`, PipeWire stream running, дельты `frame-drop-count`/`decoder-frame-drop-count`/`vo-delayed-frame-count`.
- `host.py` — `summarize_gpu`: `drm-engine-*` нс по client-ам, присутствующим на обеих границах, своя метка времени `drm_time`; отдельная метрика, не CPU. На 780M видеоблок VCN виден как движок `enc` даже при декодировании.
- `analyze.py` — quiet: Student t=10 interval, `within_exit_band` (WIN или INCONCLUSIVE внутри ±0,5 п.п.), отдельный GPU gfx вердикт; diagnostic: `DESCRIPTIVE`; `quality_gates` (Fastiplayer ≥ 99 % от fps×log-окно; эталон 0 потерь; без diagnostic-подсчёта = NOT TESTED). Эталон когорты = единственный не-fastiplayer плеер.
- `run_series.py` — план обязан иметь `protocol_version: 2`, 2 плеера (fastiplayer + эталон); quiet 3/6/9 с остановкой, diagnostic `diagnostic_repetitions` (1) в первом круге; FAIL quality-gate → exit 2; сцена с `players: ["fastiplayer"]` → NOT TESTED. Любая правка инструментов/бинарника/плана = новая серия (provenance-lock хэшей).
- `make_matrix.py` — матрица 16 + `hevc10-4k60` из одного 4K60 мастера (отрезок 45 с), ffprobe-проверка, `manifest.json` (хэши, фактический битрейт), частичные запуски дополняют манифест, чужой мастер/отрезок отвергается до кодирования. SVT-AV1 нужен `undershoot-pct=0` (иначе −20–30 % битрейта).
- Тесты: `python3 -m unittest discover -s docs/benchmarks/tools/local -p 'test_*.py'` (31; `test_protocol_v2.py`, `test_make_matrix.py` с настоящим ffmpeg), запускать вне sandbox (systemd-run, /dev/dri).

## Эталон
VLC 3.0.24 не умеет аппаратный AV1 на 780M (dav1d SW по умолчанию; `--codec=avcodec` → buffer deadlock) → решение владельца: эталон mpv 0.41 `--no-config --hwdec=auto --fs --volume=0` (выбирает Vulkan Video, `gpu-next`).

## Метод/уроки
- Серию запускать отвязанно (`setsid nohup` скрипт), ждать Monitor-ом по `series.log`; окна на весь экран ~1,5 ч — машину не трогать.
- Фуллскрин-скрипт KWin под XWayland НЕ делает Fastiplayer «фуллскрин» для winit: панель/заголовок видимы (D2). Двойной клик по видео через виртуальную абсолютную мышь `/dev/uinput` (у пользователя есть права) включает наш фуллскрин и автоскрытие.
- Быстрая разбивка по потокам — `tools/dev/run_pass.py` + `RUST_LOG=...,fastiplayer::render_frame_timing=trace` и подсчёт `frame_work_plan`/`ui_pass_reason`.

## Серия 06c (протокол v3, 2026-10-22) — итог и уроки
- Артефакты: `benchmark-output/cpu-parity-06c/` (plan-v3.json, pilot/, series-v3/, run-*.sh, environment-*.txt). План v3 = plan-v2 06 с `protocol_version: 3`, новым хэшем бинарника, тем же конфигом и window-owner. Пилот — `collect.py` на spec-ах с `capture: true` (скриншоты вне CPU-окна).
- Длительность: 68 попыток первого круга ≈ 45 мин, тихий круг 34 попытки ≈ 21 мин; до 9 пар ≈ 3 ч. summary.json пишется только в конце круга.
- Итог: quality 17/17, gfx WIN 17/17, CPU INCONCLUSIVE 17/17 без WORSE (подробно — `mem:cpu-parity/core`, `user/cpu-parity/results/06c.md`).
- Урок: коридор ±0,5 п.п. при t=10 и n ≤ 9 требует sd парных разниц ≤ ~0,12 п.п. (ширина = 3,33·sd + 0,10); mpv на 60 fps скачет между прогонами одного файла до 2,3 п.п. → коридор почти недостижим, вопрос протокола (v4) — владельцу (06d).
- Публикация: обезличенная выгрузка per-attempt (`.json.gz` в `docs/benchmarks/`) делалась скриптом в scratchpad (не в tools/local — новый .py там меняет хэши provenance серии). Поля: метрики, хэши, quality без событий, gpu engines, loadavg; без путей/PID/unit.

## SW-серия (сессия 08, 2026-10-22)
- Протокол v3 расширен необязательными полями `common` плана (без них HW-поведение прежнее): `decode` = `hardware`|`software` (mpv `--hwdec=no` и `hwdec-current == "no"` на обеих границах; Fastiplayer — `plan="ffmpeg-host-upload-wgpu"` без следов VA-API, `evidence.fastiplayer_backend_proven`; `vlc`+`software` — ошибка плана), `exit_band_percentage_points` (умолчание 0,5; SW — 1,0; `analyze.plan_exit_band`), `reference_losses` = `stop`|`annotate` (annotate: потери эталона → сцена `REFERENCE_LOSSES`, серия идёт дальше, CPU публикуется с оговоркой «при потерях у эталона»; свои потери — всегда FAIL). Эталон SW — тоже mpv (решение владельца). Артефакты: `benchmark-output/cpu-parity-08/` (plan-sw.json, конфиг = 06c с `preferred_backend = "software"`, pilot/, series-sw/). Пилот: mpv `--hwdec=no` на AV1 4K60 матрицы терял 3 кадра.

## Память GPU в протоколе (сессия 08b, 2026-10-22, решение владельца)
- mpv с DR декодирует кадры в amdgpu GTT (`/dev/dri` отображения, Rss = 0): один RSS скрывает у него 300–400 МиБ кадров на 4K. `host.gpu_memory_kib(sample)` (GTT/VRAM по уникальным `device:client-id` DRM fdinfo, `parse_drm_memory_kib`) → `collect.summarize` метрики `gpu_gtt_kib`/`gpu_vram_kib` (mean/sampled_max/minimum; нет DRM-клиентов — метрики нет) → `analyze.memory_summary`: `gtt_mean_mib`, `vram_mean_mib`, `rss_plus_gtt_mean_mib` (старые серии → `null`). `PROTOCOL_VERSION` не менялся (поля только добавлены). Оговорка: у Fastiplayer импортированные на время GPU-копии блоки кадров (`userptr`) драйвер тоже считает в GTT, хотя они уже в RSS — RSS+GTT завышен на 2–3 кадра в полёте.
- Серия 08b — `benchmark-output/cpu-parity-08b/` (тот же план, что 08, новый бинарник).

## Драйверы i915 и amdgpu (сессия 10, 2026-10-23)
- Счётчики DRM fdinfo зависят от драйвера: amdgpu — движок `gfx`, память `drm-memory-gtt`/`drm-memory-vram`; i915 — движок `render`, память `drm-resident-system0` (→ GTT-колонка) / `drm-resident-local0` (→ VRAM). Таблицы `RENDER_ENGINE_NAMES`, `GPU_MEMORY_FIELDS` + `render_engine_busy_percent`, `client_memory_kib`, `gpu_memory_summary` в `host.py`; `analyze.gpu_memory_metrics` пересчитывает память из сырых samples (только если в попытке уже есть `gpu_*` и у всех samples есть `drm`), поэтому серии i915, снятые до правки, исправляются без перезамера. Тесты `DriverNamingTests` в `test_protocol_v2.py` (55 всего). Новый драйвер (xe, nouveau…) — добавить имена в эти таблицы, иначе GPU-метрики молча пропадут.
- HW-путь: VA-поверхности видны двум DRM-клиентам процесса (VA-API и Vulkan, у i915 они в `drm-shared-*`) → сумма по клиентам считает их дважды. Ручная оценка «общее один раз» = Σ(resident − shared) + max(shared); в `analyze.py` не встроена (бэклог).

## Диагнозы (для 06a/06b)
- D1 (ЗАКРЫТ в 06b: позиция в модели только при видимой панели): `UiFrameModel::build` всегда включал `quantize_timeline_position` (пиксель ползунка) независимо от видимости таймлайна → ~25–28 полных egui-проходов/с на 45-с клипах даже со скрытой панелью; главный поток на 30 fps почти не дешевле 60 fps.
- D2: см. выше. ЗАКРЫТ в 06b (2026-10-22): протокол v3 — `collect.py::fastiplayer_command` = `--fullscreen`, `PROTOCOL_VERSION = 3` в `host.py`, план серии обязан объявить 3 (`mem:cpu-parity/ui-visible-changes-fullscreen-s06b-2026-10-22`). Решение владельца 2026-10-21: протокол v3 входит в фуллскрин CLI-флагом `--fullscreen` (сессия 06b), флаг передаётся и в уже открытое окно (D-Bus `platform_data`); uinput-клик и распознавание WM-фуллскрина отклонены. Лишние VA-API декодеры на старте — подзадача 06b, сабагент Opus 5.5 / medium. Файлы сессий: `user/cpu-parity/06a-*.md`, `06b-*.md`, `06c-*.md`.
- D3: повтор+пропуск на 60 fps (3/16 прогонов, любой кодек). ЗАКРЫТ в 06a: виноват не планировщик, а latest-slot + чтение слота по vsync под XWayland/FIFO (кадр брался до ожидания буфера swapchain) — `mem:cpu-parity/frame-selection-cadence-s06a-2026-10-21`. Для повторной проверки quality-gate без mpv: спецификации `benchmark-output/cpu-parity-06/series-v2c/specs/*-diagnostic-01-fastiplayer.json` + замена бинарника → `collect.py` → `analyze.attempt_quality`.
