# GLES2-рендер для старого железа (issue #6) — состояние плана

План, решения владельца, архитектура и сессии: `user/gles2-renderer/README.md` (+ `00`…`13`, отчёты `results/NN.md`). Спланировано 2026-10-23, код не менялся (HEAD `cba51ace`).

## Решения владельца (2026-10-23)
- Свой рендер: egui-художник, видео, цвет, HDR→SDR, субтитры, маска углов — сами. Внешнее только `glow` + `khronos-egl` (уже в Cargo.lock через wgpu-hal: glow 0.17, khronos-egl 6.0); `egui_glow` не используем.
- Переиспользуемые crate-ы `gles2-egui` (EGL, возможности, egui, кадр, fence) и `gles2-video` (YUV, импорт, цвет, bitmap-оверлеи) — в workspace, guardrail запрещает им зависеть от crate-ов плеера. Адаптер плеера — `render-gles2-shell`.
- Шов приложение↔рендер — trait (`render-shell-api`: `RenderShell` + `VideoFrameImport`), под будущие Windows/wgpu и свой DX10. Рендер сам владеет импортом кадров и освобождением после GPU; приложение держит `Box<dyn RenderShell>` и непрозрачные ключи; выбор через каталог рендеров платформы.
- Auto: аппаратный Vulkan с нужными расширениями → wgpu; нет Vulkan / только CPU-Vulkan (lavapipe) / wgpu не стартовал → GLES2; метка незавершённого старта → следующий запуск пропускает упавший рендер + уведомление. Смена в настройках — после перезапуска. `render.opengles.simple_ui` удаляется (противоречит паритету).
- HW строго zero-copy (DMA-BUF → EGLImage, раздельные плоскости R8/GR88, свой шейдер цвета); SW ≤ 1 копия (без unpack_subimage — текстура шириной в stride + обрезка в шейдере), источник плоскостей сразу допускает 0 копий; эксперимент udmabuf — сессия 12.
- Паритет с wgpu обязателен; компромисс только 10-бит HW. SW 10/12-бит — через LUMINANCE_ALPHA (байты) + сборка в шейдере (highp).
- Критерий на T410 (Ironlake, GL 2.1/GLES 2.0, без Vulkan; VA-API i965 только H.264/MPEG-2 ≤2048): против mpv (`vo=gpu` GL) и VLC 3 в лучших режимах — 0 потерь и CPU не выше на H.264 720p/1080p HW/SW с 30 и 60 fps; стресс-лесенка — наш потолок выше; память не хуже. Сначала 780M, потом T410 по ssh (алиас `t410`); на T410 свежая система с Wayland-сессией. Тестируем в ОБОИХ режимах — нативный Wayland EGL и XWayland (решение владельца), соперники в том же режиме.
- Аппаратный MPEG-2 — бэклог по замеру сессии 11.

## Факты разведки, на которые опирается план
- wgpu на Ironlake невозможен (GL-бэкенду нужен GLES 3.0/GL 3.3; наш shell включает только `Backends::VULKAN`, CPU-адаптеры не отсекает).
- В коде уже есть: `RenderBackendKind::OpenGles`, `RenderProfile {Auto, Vulkan, OpenGles}` (default Vulkan), `OpenGlesConfig {enabled, simple_ui}`, отказ `UnsupportedProfile` в `app-egui/src/renderer_recreation.rs:~237`; контракт субтитров для GLES2 — `mem:subtitles/on-screen-s03`, `mem:subtitles/pgs-s11`.
- `app-egui` связан с wgpu: ~30 сигнатур `&render_wgpu_shell::Renderer`, ~25 мест с сырыми device/queue (`VideoPipelineRebuildRequest`), материализаторы и `wrap_video_backend_for_wgpu_submission` в `AppState`; `VideoPipelinePlan` только wgpu-варианты; capability-core должен видеть возможности только активного рендера.
- Расширения crocus/Ironlake не подтверждены — сессия 00 (живой eglinfo/es2_info/vainfo + стенд VA→DMA-BUF→EGLImage).

## Статус сессий
- Планирование — DONE 2026-10-23. Следующая — 00 (разведка T410 и эталон mpv/VLC).
