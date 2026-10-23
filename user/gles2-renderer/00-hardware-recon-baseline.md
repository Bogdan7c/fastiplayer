# Сессия 00: разведка железа T410 и эталон соперников

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 00 плана `user/gles2-renderer/`: подтверди на живом T410 (по ssh, алиас `t410`) возможности драйверов EGL/GLES2/VA-API, проверь цепочку «VA-поверхность → DMA-BUF → EGLImage», найди лучший рабочий режим mpv и VLC 3 и сними их эталон, включая 60 fps и стресс-лесенку. Код плеера не меняется. Сначала прочитай `user/gles2-renderer/README.md`.

## Зачем

Всё, что мы знаем о Ironlake, — из исходников и форумов. Расширения crocus не подтверждены. Если на T410 нет, например, `EGL_EXT_image_dma_buf_import`, сессия 07 меняет форму, а уровни возможностей в README нужно поправить **до** проектирования контракта. Эталон соперников нужен до того, как писать рендер: это и есть планка критерия успеха.

## Что делать

1. **Доступ и окружение.**
   - `ssh t410 true`; модель CPU (`lscpu`), RAM, ядро, дистрибутив, версия Mesa, композитор Wayland-сессии (со слов владельца — Wayland; какой именно и на какой GL он сам рисует на Gen5) и доступен ли XWayland.
   - Как запускать GUI из ssh в живой сессии: переменные `DISPLAY`/`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`, `systemd-run --user`, `loginctl enable-linger`.
   - Права на `/dev/dri/renderD128` и `/dev/udmabuf` (последний нужен сессиям 07 и 12).
2. **Возможности драйверов** (полные дампы — в scratchpad, в отчёт — таблица):
   - `eglinfo -B` и полный `eglinfo` для платформ x11/wayland/surfaceless;
   - `es2_info` (или `glxinfo -B` + ES-профиль). Проверить: `GL_EXT_texture_rg`, `GL_EXT_unpack_subimage`, `GL_OES_element_index_uint`, `GL_OES_vertex_array_object`, `GL_OES_EGL_image`, `GL_OES_EGL_image_external`, `GL_FRAGMENT_PRECISION_HIGH`, `GL_MAX_TEXTURE_SIZE`;
   - EGL: `EGL_EXT_image_dma_buf_import`, `_modifiers`, `EGL_KHR_fence_sync`, `EGL_EXT_buffer_age`, `EGL_KHR_swap_buffers_with_damage`, `EGL_EXT_create_context_robustness`, EGL-конфиги с альфой (прозрачное окно);
   - `vainfo`: профили и entrypoint-ы (ожидаем H.264 + MPEG-2).
3. **Цепочка zero-copy без плеера.** Маленький одноразовый стенд в scratchpad (C или Python + ctypes, не в репозиторий):
   - декодировать один кадр H.264 через VA-API;
   - `vaExportSurfaceHandle(DRM_PRIME_2)` с `COMPOSED_LAYERS` и с `SEPARATE_LAYERS`, записать fourcc, модификатор, число объектов/слоёв, pitch/offset;
   - импортировать плоскости в EGL как `DRM_FORMAT_R8` + `DRM_FORMAT_GR88` (и NV12 целиком как external image — для сравнения), отрисовать, сделать readback и сверить с программным декодом того же кадра.

   Это главный риск плана, и ответ на него нужен до сессии 01. Параллельно: `mpv --hwdec=vaapi --vo=gpu --gpu-api=opengl` и его лог interop (zero-copy или `vaapi-copy`).
4. **Тот же набор проверок на основной машине (780M/radeonsi) в GLES2-контексте.** Это вторая целевая среда для разработки и тестов. Плюс EGL surfaceless + llvmpipe — среда для пиксельных тестов в CI (проверить, что GLES2-контекст поднимается без окна).
5. **Метрики на Intel Gen5.**
   - Что отдаёт `/proc/<pid>/fdinfo` i915 на этом поколении: `drm-engine-render/video/copy`? `drm-total-system`?
   - Работает ли `intel_gpu_top`?
   - Есть ли датчик мощности (батарея `power_now` на разряде; RAPL на Arrandale нет).

   Вывод: какие колонки протокола v3 на T410 будут `NOT TESTED`.
6. **Фикстуры для T410** (генератор — расширить `docs/benchmarks/tools/local/make_matrix.py` или отдельный пресет, файлы в `test-assets/`, не в git):
   - обычные: H.264 High 720p30, 720p60, 1080p30, 1080p60 (жизненный битрейт);
   - стресс-лесенка по возрастанию нагрузки: H.264 1080p60 высокий битрейт; HEVC 720p30 → 1080p30 → 1080p60; VP9 720p30 → 1080p60; AV1 720p30 → 1080p30 → 1080p60; HEVC Main10 1080p30 (программный 10-бит + HDR→SDR — нагрузка и на рендер); MPEG-2 576i/1080i (для решения по бэклогу).
7. **Эталон соперников на T410.**
   - **mpv:** перебрать рабочие режимы (`--vo=gpu --gpu-api=opengl` c `--gpu-context=wayland` и `x11egl` под XWayland, `--hwdec=vaapi` и `vaapi-copy` и `no`, `--vo=dmabuf-wayland` как отдельный кандидат: он показывает кадр композитором без GL. Записать, во что он обходится композитору). Лучший режим = 0 потерь при минимальном CPU на сцене. Потери — через JSON IPC (`frame-drop-count`, `decoder-frame-drop-count`, `vo-delayed-frame-count`).
   - **VLC 3:** его GL/GLES2-вывод и VA-API, потери — через `--verbose` статистику или rc-интерфейс `stats`.
   - На каждой сцене: CPU процесса (среднее и по потокам), RSS, потери кадров, «доиграл / не доиграл плавно». Для лесенки — потолок каждого соперника.
   - Окно, повторы и условия (сеть, зарядка, яркость, `cpupower`-governor) записать в отчёт.

## Чего не делать

- Не менять код плеера и не коммитить стенд. В репозиторий идут только `results/00.md`, правки README плана и, при необходимости, генератор фикстур.
- Не делать выводов о нашей производительности: плеер на T410 сейчас вообще не стартует (Vulkan нет). Зафиксировать это как «до»: какая ошибка, какое окно видит пользователь (`FatalStartupError::graphics_unavailable`) и не выбирает ли wgpu lavapipe, если тот установлен.

## Критерий выхода

- Таблица возможностей T410 и 780M (GLES2) с отметкой «подтверждено живьём».
- Цепочка VA → DMA-BUF → EGLImage доказана readback-ом (или доказано, что её нет, с причиной и предложением, как меняются уровни и сессия 07).
- Эталон mpv/VLC: лучший режим по каждой сцене, CPU, RSS, потери, потолок лесенки.
- README плана обновлён, если факты расходятся с разведкой. Вопросы владельцу (если уровни меняются) — через AskUserQuestion.

Memories: `mem:cpu-parity/core` (метод замера), `mem:cpu-parity/hw-qualification-s06-2026-10-21` (протокол и quality-gate), `mem:video-vaapi/core`, `mem:testing/media-fixtures`.
