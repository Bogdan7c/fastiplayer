# Сессия 03: фундамент `gles2-egui` — EGL, возможности, кадр

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 03 плана `user/gles2-renderer/`: создай переиспользуемый crate `gles2-egui` (без зависимостей от crate-ов плеера) с EGL-контекстом GLES 2.0 на окне winit (X11 и Wayland, прозрачность), определением возможностей драйвера, swap/vsync, fence и обработкой потери контекста, плюс headless-стенд для пиксельных тестов. Сначала прочитай `user/gles2-renderer/README.md`, `results/00.md` и `results/01.md`.

## Зачем

Всё остальное (egui, видео, субтитры) стоит на этом слое. Ошибки здесь — это чёрный экран на чужой машине. Поэтому слой маленький, строго типизированный и покрыт тестами без окна.

## Что сделать

1. **Crate `gles2-egui`**, модуль `context`. Зависимости: `glow`, `khronos-egl` (версии как в `Cargo.lock`: 0.17 / 6.0, чтобы не плодить дубли), `raw-window-handle`, `wayland-egl` (для `wl_egl_window`), `thiserror`, `log`/`tracing`. Сверить API по Context7.
   - Загрузка `libEGL` динамически (`khronos-egl` `dynamic`): если библиотеки нет, это типизированная ошибка `EglUnavailable`, а не паника при старте.
   - Платформа по `RawDisplayHandle`: Wayland → `EGL_PLATFORM_WAYLAND_KHR` + `wl_egl_window`; X11/Xlib → `EGL_PLATFORM_X11_KHR`, конфиг под visual окна (`EGL_NATIVE_VISUAL_ID`), иначе прозрачность сломается; surfaceless/pbuffer → для тестов.
   - Контекст `EGL_CONTEXT_CLIENT_VERSION = 2`. Robustness (`EGL_EXT_create_context_robustness`), если есть.
   - Конфиг с альфа-каналом для прозрачного окна (паритет скругления углов). Если альфы нет, рендер честно сообщает «углы квадратные» — так же, как wgpu при `Opaque` (`surface_alpha.rs`).
2. **`Gles2Capabilities`** — что реально умеет драйвер: версия, рендерер/вендор, расширения GL и EGL, `MAX_TEXTURE_SIZE`, highp во фрагментном шейдере, импорт DMA-BUF (+ список форматов/модификаторов через `eglQueryDmaBufFormatsEXT`/`ModifiersEXT`, где есть). Из этого набора выводятся уровни README. Без эвристик по имени GPU.
3. **Кадр и показ.**
   - `begin_frame` / `end_frame` / `swap`, `eglSwapInterval(1)` по умолчанию; настройки показа берутся из конфигурации вызывающего, а не захардкожены.
   - Resize: `wl_egl_window_resize` / пересоздание surface.
   - Тайминги стадий в нейтральной структуре, совместимой по именам с `RenderFrameStageTimings`, где это имеет смысл (`surface_present` = swap).
4. **`GpuFence`.**
   - `EGL_KHR_fence_sync`: создать после отправки кадра, `glFlush`, ожидание с таймаутом из любого потока.
   - Без расширения — фолбэк `glFinish` перед объявлением «GPU закончил». Явный счётчик `fence_fallback_finishes`, чтобы цена фолбэка была видна в замере.
   - Безопасность: sync-объекты удаляются ровно один раз (тесты владения).
5. **Потеря контекста.**
   - `EGL_CONTEXT_LOST`, `GL_GUILTY/INNOCENT_CONTEXT_RESET` (если robustness есть) → типизированное `ContextLost`.
   - Вызывающий (позже `render-gles2-shell`) превращает его в существующий путь «device lost» (`release_submitted_frames_after_device_lost`).
6. **Headless-стенд для тестов.** EGL surfaceless (`EGL_MESA_platform_surfaceless`) или pbuffer → offscreen FBO → `glReadPixels`. На машине разработчика — железо, в CI — llvmpipe. Если EGL недоступен, тест пропускается с причиной в выводе. Проверить, что CI (`mem:ci/github-actions`) может поставить Mesa EGL. Если нет — `#[ignore]` с инструкцией запуска, решение владельцу.
7. **Пример** `examples/clear_window.rs`: окно winit, заливка цветом, вывод `Gles2Capabilities`. Это первый ручной тест на T410 по ssh.

## Факты разведки 00 (`results/00.md`)

- **Строгий ES 2.0.** radeonsi и llvmpipe на запрос «GLES 2» отдают контекст ES 3.2. `Gles2Capabilities` строится только из ES 2.0 + строк расширений. Возможности ES3 (`texelFetch`, целочисленные текстуры, `GL_RED` без `EXT_texture_rg`, `#version 300 es`) не используются нигде, даже если есть. Иначе пиксельные тесты на 780M и в CI зелёные, а на T410 чёрный экран. Тест: шейдеры компилируются как `#version 100`, а набор возможностей на fake-строках Ironlake совпадает с таблицей 00.
- T410: EGL-конфиги с альфой есть (Wayland 8; X11 — 4 с 32-битным ARGB visual), `buffer_age`, `swap_buffers_with_damage`, robustness, `KHR_fence_sync` и `ANDROID_native_fence_sync` есть, surfaceless работает (на железе и на llvmpipe).
- **GPU Ironlake иногда виснет под нагрузкой** (два случая в 00, один со сбросом контекста). Потеря и сброс контекста — не теоретический путь: пример и `render-gles2-shell` обязаны его переживать без падения процесса.

## Безопасность

FFI EGL/GL — отдельный модуль `context/safety.rs` с `// SAFETY:` на каждом `unsafe`. Владение `EGLSurface`/`EGLContext`/`EGLSync` в RAII-типах с `Drop`. Порядок разрушения: surface → context → display (тест). Без `unwrap/expect` в production.

## Guardrail

`gles2-egui` в `scripts/check-refactor-guardrails.py`: allowlist внешних зависимостей. Запрещены все crate-ы workspace, кроме будущих `gles2-*`.

## Тесты (результат, а не вызов)

- headless: очистка известным цветом → readback точно этот цвет; два кадра подряд → второй цвет;
- `Gles2Capabilities` на fake-строках расширений → правильные уровни (таблица);
- fence: команда отправлена → fence сигналит, освобождение вызвано ровно один раз; фолбэк без расширения считает `glFinish`;
- RAII: двойное разрушение невозможно, порядок соблюдён (fake-EGL);
- ошибка создания (нет libEGL, нет конфига, нет GLES2) → нужный вариант ошибки.

## Критерий выхода

- Пример открывает окно и заливает его на 780M и на T410, **в обоих режимах**: нативный Wayland и XWayland (решение владельца). Прозрачность углов проверена в обоих.
- Вывод `Gles2Capabilities` на обеих машинах совпадает с таблицей `results/00.md`.
- Тесты зелёные локально. Решение по CI записано.

Memories: `mem:render-video/core` (разделы про surface/present и Mesa WSI 07a), `mem:dependency-patches/core` (winit-patch), `mem:ci/github-actions`, `mem:panic-invariant-policy`.
