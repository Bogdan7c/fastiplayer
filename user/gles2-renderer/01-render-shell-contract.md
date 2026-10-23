# Сессия 01: контракт `render-shell-api`

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 01 плана `user/gles2-renderer/`: спроектируй и создай нейтральный crate `render-shell-api` — контракт «приложение ↔ рендер», под который потом встанут wgpu, GLES2 и будущий DirectX 10. До кода покажи владельцу описание контракта простым языком. Поведение приложения в этой сессии не меняется. Сначала прочитай `user/gles2-renderer/README.md` и `results/00.md`.

## Зачем

Сейчас `app-egui` знает конкретный `render_wgpu_shell::Renderer`:
- ~30 сигнатур принимают `&Renderer`;
- ~25 мест достают сырые `instance/adapter/device/queue` (например `VideoPipelineRebuildRequest`, `state/video_backend.rs:46-52`);
- материализаторы кадров (`WgpuFrameTextureViewMaterializer`) и release-обёртка (`wrap_video_backend_for_wgpu_submission`) живут в `AppState`.

Решение владельца 3: общий интерфейс, рассчитанный на Windows/DX10. Значит, приложение не должно видеть ни GPU-API, ни способ импорта кадров.

## Что уже нейтрально (не переделывать)

- `render-core`: `RenderBackendKind { Wgpu, OpenGles }`, `RenderCapabilities` (`supported_frame_contracts` — источник истины поддержки), `RenderDiagnostics`, `RenderLiveSettings` + trait `RenderLiveSettingsAdapter`, `RenderViewport`, `PictureLayout`/`PictureRect`.
- `video-backend-api`, `video-present-core` (`VideoFrameLease`, release sink), `video-core`-дескрипторы.
- `config`: `RenderProfile { Auto, Vulkan, OpenGles }`.
- Обобщённые по `<Materializer, SubmissionBinding>` `video_pipeline_candidate*`.

## Что спроектировать

1. **`trait RenderShell`** (object-safe: приложение держит `Box<dyn RenderShell>`). Методы по намерению:
   - `family() -> RendererFamily`, `capabilities() -> &RenderCapabilities`, `diagnostics()`;
   - `resize(...)`, `acquire_surface_frame(screen) -> SurfaceFrameAcquire`;
   - `can_reuse_retained_ui(screen)`;
   - `render_frame(RenderFrameInput) -> RenderFrameOutcome`;
   - `live_settings() -> &mut dyn RenderLiveSettingsAdapter`;
   - `wait_for_gpu_idle(timeout) -> Result<_, RendererGpuDrainError>`;
   - `frame_stage_timings()` / `hot_path_stats()` в нейтральной форме;
   - `video_frames() -> &mut dyn VideoFrameImport`.

   Набор сверить с реальным использованием (отчёт разведки — `results/00.md` и поиск субагентом по `app-egui`).
2. **`trait VideoFrameImport`** — рендер владеет импортом и освобождением:
   - `bind_video_backend(StartedVideoBackend, VideoPipelinePlanKind) -> Result<BoundVideoBackend, _>` — оборачивает provider так, что освобождение кадра идёт только после GPU (wgpu: `on_submitted_work_done`; GLES2: EGL fence; DX: fence);
   - `materialize(&VideoFrameLease) -> FrameMaterialization`. Исходы `Ready(RenderableFrameKey)`, `Busy`, `Missing`, `Unsupported(reason)`, `Error` остаются **различимыми** (правило 6). Ключ непрозрачный, текстуры живут в рендере;
   - `release_after_device_lost()`, `rebind_after_recreation(...)`.

   Место Busy-фолбэка «показать прошлый кадр» остаётся в приложении (`shared_frame_materialization.rs`): оно решает о показе, а не об импорте.
3. **Нейтральные типы кадра** переезжают из `render-wgpu-shell/src/frame.rs` в контракт: `RenderFrameInput`, `RenderFrameOutcome`, `RenderFrameDropReason`, `EguiFrameContent::{Fresh, ReusePrevious}`, `RenderScreenDescriptor`, `RenderFrameStageTimings` (имена стадий сохранить — их читает `agg_timing.py`), `WindowCornerMask`, `SubtitleOverlayFrame`. `AcquiredSurfaceFrame` становится непрозрачным билетом: рендер хранит реальный буфер внутри. Инвариант 06a «нельзя рисовать без полученного буфера» сохраняется типом (`#[must_use]`, конструктор только у рендера).
4. **`RendererFamily`, `RendererProbe`, `RendererCreateError`** — словарь каталога:
   - `Usable`, `Unusable { reason: RendererUnusableReason }`; варианты причин: `NoHardwareAdapter`, `SoftwareAdapterOnly`, `MissingRequiredExtension(..)`, `PreviousStartupCrashed`, `CreationFailed(..)`;
   - на Windows словарь не меняется, добавляются только семейства.
5. **Conformance-набор** — функции-проверки контракта, которые потом прогоняются для каждого реального рендера:
   - нельзя `render_frame` без билета;
   - `ReusePrevious` без удержанного кадра → `RetainedUiUnavailable`;
   - `materialize` для неподдерживаемого контракта → `Unsupported`, а не `Error`;
   - освобождение ровно один раз.

   В этой сессии гоняются на fake-рендере.

## Решения, которые принять с владельцем

- Где живёт каталог рендеров (чистая функция выбора + фабрики): модуль `app-egui::render_backend` (композиция) или маленький crate. Рекомендация — модуль в `app-egui`: каталог зависит от конкретных рендеров, а это работа composition root.
- Поведение при `Unsupported` формата на активном рендере в `Auto` (переход на SW-декод через существующий выбор `video_pipeline_selector`) — подтвердить, что оно не меняется.

## Чего не делать

- Не переводить `app-egui` на контракт (это сессия 02). Здесь только crate, типы, тесты на fake и guardrail.
- Не менять поведение wgpu. Перенос типов кадра делается с реэкспортом из `render-wgpu-shell`, чтобы `app-egui` не пришлось трогать в этой сессии.

## Тесты

- Conformance-набор на fake-рендере.
- Типы-исходы: каждый вариант различим, «absent vs busy vs fatal» не схлопывается.
- `scripts/check-refactor-guardrails.py`: `render-shell-api` не зависит от `wgpu`, `ash`, `glow`, `khronos-egl`, `winit`, `egui-wgpu`, конкретных бэкендов и `player-core`; нейтральные crate-ы не зависят от будущих `render-gles2-shell`/`gles2-*`.

## Критерий выхода

- Владелец одобрил описание контракта.
- Crate собран, guardrail-ы и conformance на fake зелёные, `scripts/ci-checks.sh all` зелёный.
- В `results/01.md` — таблица «метод контракта ↔ где сейчас используется в app-egui ↔ кто реализует для wgpu/GLES2/DX».

Memories: `mem:render-video/core`, `mem:render-video/controlled-renderer-recreation-s08c`, `mem:app-egui/render-on-change-s04-2026-10-20`, `mem:cpu-parity/frame-selection-cadence-s06a-2026-10-21`, `mem:render-core/module-layout-s22`.
