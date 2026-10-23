# Сессия 02: wgpu-рендер за контрактом

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 02 плана `user/gles2-renderer/`: переведи `app-egui` с конкретного `render_wgpu_shell::Renderer` на `Box<dyn RenderShell>` из `render-shell-api`, перенеси материализаторы кадров и release-обёртку внутрь wgpu-рендера, заведи каталог рендеров с одним кандидатом. Поведение — один в один, доказать замером «до/после» и quality-gate. Сначала прочитай `user/gles2-renderer/README.md` и `results/01.md`.

## Зачем

Это единственная сессия, которая трогает рабочий путь всех пользователей. Её задача — чтобы после неё приложение умело работать с любым рендером, а wgpu-путь не изменился ни на кадр. По правилу 8 `AGENTS.md` это отдельное осознанное изменение границы, без новых фич.

## Что делать

1. **wgpu-рендер реализует `RenderShell` и `VideoFrameImport`.**
   - `render-wgpu-shell::Renderer` (или тонкая обёртка в том же crate-е) реализует контракт.
   - `DmaBufWgpuFrameMaterializer` / `HostPlanarWgpuFrameMaterializer` и `wrap_video_backend_for_wgpu_submission` создаются и хранятся **внутри** рендера (`bind_video_backend`), а не в `AppState` (`state.rs:21-24,232,235`).
   - Кэш импорта, счётчики (`DmaBufImportStats`, `VideoHotPathStats`), барьеры FOREIGN (cpu-parity 03) и host-import (08b) не меняются.
2. **`app-egui` держит `Option<Box<dyn RenderShell>>`.**
   - ~30 сигнатур с `&Renderer` переходят на `&dyn RenderShell` / `&mut dyn RenderShell` (механически, без изменения логики).
   - `VideoPipelineRebuildRequest` и пути пересборки (`state/video_backend.rs`, `strong_media_open*`, `suspended_media_resume.rs`, `settings_runtime_adapter.rs:533`, `app_shell/mod.rs:354-357`) больше не несут `instance/adapter/device/queue`: им нужен только `&mut dyn VideoFrameImport`.
3. **`VideoPipelinePlan` развязать с семейством рендера.**
   - Сейчас это `VaapiDmaBufWgpu` / `FfmpegHostUploadWgpu` (`video_pipeline_selector.rs:31-43`). Должно стать «маршрут декода (`VaapiDmaBuf`, `FfmpegHostUpload`) × семейство рендера».
   - Строки плана в логах, по которым классифицируют прогоны инструменты (`plan="ffmpeg-host-upload-wgpu"`, «VA-API Vulkan DMA-BUF»), сохранить байт-в-байт для wgpu: `docs/benchmarks/tools/local/evidence.py:153-163`, `t480s/collect.py:77-78`.
   - `CandidateVideoPipelineResourceDriver` (уже обобщён) получает реализацию через контракт.
4. **Каталог рендеров** `app-egui::render_backend`:
   - чистая функция `select_renderer(profile, probes, startup_marker) -> RendererSelection` и список кандидатов платформы (сейчас один: wgpu/Vulkan);
   - создание из `app_shell/mod.rs:283` и `renderer_recreation.rs:246` идёт через каталог;
   - здесь же убрать дубль маппинга `VulkanPresentMode → SurfacePresentSettings` (`render_settings.rs:12-30` и `renderer_recreation.rs:380-390`) — он уезжает в фабрику wgpu-кандидата. Это не косметика: каталог должен быть единственным местом создания.
5. **Пересоздание рендера** (`renderer_recreation.rs`, `RendererLifecycleCoordinator`) работает через контракт. Ветку `RenderProfile::OpenGles → UnsupportedProfile` (`renderer_recreation.rs:237`) оставить до сессии 09.
6. **Guardrail**: `app-egui` больше не зависит от `wgpu` напрямую (кроме тестов, если без этого никак — с явным объяснением). `render-wgpu-video` остаётся без `app-egui`/`egui`/`winit`.

## Размер модулей

`app_shell/mod.rs` = 798 строк, `frame_prepare.rs` = 1467 (legacy), `state.rs` = 776, `renderer_recreation.rs` = 754. В эти файлы — только замена типов в одну строку. Новый код — в новые модули (`render_backend/`). Ratchet `module-size-baseline.json` обновить, если legacy-файл уменьшился.

## Тесты

- Conformance-набор из сессии 01 прогоняется на **реальном** wgpu-рендере (GPU-тест, явный пропуск без адаптера).
- Существующие GPU-тесты рендера (`egui_compositor`, `subtitle_overlay`, `host_memory_import`, udmabuf `#[ignore]`) зелёные без правок ожиданий.
- `select_renderer`: профиль `Vulkan`/`Auto`/`OpenGles` × пробы × метка — таблица исходов (OpenGles пока → понятная ошибка «рендер не собран», как сейчас).
- Тест, что `app-egui` не видит GPU-типов: guardrail.

## Замер (обязательно)

- Release, 2 круга с чередованием «до/после»: HW H.264 1080p60, HW 4K30, SW H.264 1080p60, SW AV1 4K60 (`run_pass.py` + `agg_timing.py`). CPU по потокам, gfx, RSS, уникальные кадры, `drops_*`.
- Допуск — шум дня (см. `mem:cpu-parity/core`). Систематический рост главного потока (виртуальные вызовы, лишние `Arc`) — найти и убрать, а не принять.
- Ручная приёмка: seek/scrub/pause/resume, переключение HW↔SW, смена файла, resize, фуллскрин, Apply настроек рендера (пересоздание), субтитры, скруглённые углы.

## Критерий выхода

- Пользователь не видит разницы, замер в шуме, quality-gate без изменений.
- В `app-egui` нет `render_wgpu_shell::Renderer`, `wgpu::Device/Queue`, `Wgpu*Materializer` вне фабрики каталога.
- Memories обновлены: `mem:render-video/core` (кто владеет материализацией), новая `mem:gles2-renderer/render-shell-contract`.

Memories: `mem:app-egui/state-split`, `mem:render-video/controlled-renderer-recreation-s08c`, `mem:app-egui/render-on-change-s04-2026-10-20`, `mem:investigations/playlist-session-00c-candidate-video-resources-2026-07-13`, `mem:render-video/host-planar-staging-belt-upload`.
