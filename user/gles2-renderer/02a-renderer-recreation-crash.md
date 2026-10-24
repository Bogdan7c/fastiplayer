# Сессия 02a: живое пересоздание рендера падает на Apply настроек Vulkan

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 02a плана `user/gles2-renderer/`: найди и устрани корневые причины падения при Apply настроек `[render.vulkan]` (пересоздание рендера), не сломав откат транзакции пересоздания. Сначала прочитай `user/gles2-renderer/README.md` и `results/02.md`.

## Что найдено в приёмке сессии 02 (24.10.2026)

Сценарий: открыть настройки → render → vulkan → поменять `Max frame latency` (или present mode) → Apply. Плеер падает с кодом 139. **Одинаково падают сборка до сессии 02 (`27be1df1`) и после неё**, то есть это давний дефект, а не регрессия шва. Машина: Radeon 780M, Mesa 26.2, KWin 6.8, нативный Wayland.

Лог (оба бинарника):

```
INFO new{surface_present=SurfacePresentSettings { present_mode: Fifo, max_frame_latency: 2, .. }}: Renderer is fully initialized
wp_fifo_manager_v1#60: error 0: Attempted to create a second fifo surface for the wl_surface
thread 'main' panicked at egui-wgpu-0.36.2/src/renderer.rs:672:18:
Tried to update a texture that has not been allocated yet.
```

Две независимые причины:

1. **Две Vulkan-surface на одном окне.** `RendererLifecycleCoordinator` сначала полностью собирает рендер-кандидат (новая `wgpu::Surface` на том же `wl_surface`), а старый рендер ещё жив. Так устроен откат: при любой ошибке остаётся старый рендер. Mesa WSI на Wayland создаёт на каждую surface объект `wp_fifo_v1`, а протокол запрещает второй такой объект на одном `wl_surface` — композитор рвёт соединение.
2. **Пустой набор текстур интерфейса у нового рендера.** Новый `EguiCompositor` создаётся без текстур, а приложение передаёт egui только изменения текстур с прошлого кадра (`TexturesDelta`). Атлас шрифтов и картинки, загруженные в старый рендер, новый не получает, и `egui_wgpu::Renderer::update_texture` для частичной дельты паникует.

## Что сделать

- Описать владельцу варианты для причины 1 простым языком до кода. Решение меняет устройство транзакции (правило 9), например:
  - (а) снимать surface старого рендера до создания новой: рендер умеет «отпустить окно», откат пересоздаёт surface старой конфигурации;
  - (б) переконфигурировать surface в рамках того же рендера: present mode и latency меняются через `surface.configure`, без нового device. Тогда пересоздание всего рендера остаётся для смены device;
  - (в) свой вариант по итогам Context7 (wgpu 30, `wp_fifo_v1`).
- Причина 2: после установки нового рендера приложение обязано передать ему полный набор текстур egui (`egui::Context` / `TexturesDelta` с полными `ImageDelta`), а не только изменения. Сверить API egui 0.36 через Context7.
- Оба исправления — через контракт `render-shell-api`, чтобы их наследовал GLES2 (сессия 09): у EGL на одном окне тоже одна surface.
- Тесты:
  - пересоздание рендера на настоящем wgpu без окна: после него кадр с интерфейсом рисуется без паники, а пиксели совпадают с кадром до пересоздания;
  - откат при отказе создания кандидата сохраняет показ;
  - ручная приёмка Apply на Wayland и XWayland, видео HW и SW, субтитры видны.

## Критерий выхода

Apply любой настройки `[render.vulkan]` во время воспроизведения HW и SW не роняет плеер, видео, интерфейс и субтитры продолжаются. Откат при ошибке создания кандидата работает. `ci-checks all` зелёный.

Memories: `mem:render-video/controlled-renderer-recreation-s08c`, `mem:gles2-renderer/render-shell-contract`, `mem:render-video/core` (раздел egui-wgpu frame lifecycle).
