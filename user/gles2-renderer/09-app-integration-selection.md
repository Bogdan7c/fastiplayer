# Сессия 09: встраивание в приложение и выбор рендера

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 09 плана `user/gles2-renderer/`: собери `render-gles2-shell` в полноценную реализацию `RenderShell`, добавь его в каталог рендеров Linux, сделай автовыбор (аппаратный Vulkan → wgpu, иначе GLES2; программный Vulkan и падение на старте → GLES2 с уведомлением), выбор в настройках с применением после перезапуска и миграцию конфига. Докажи функционально, что видео доходит до экрана через GLES2. Сначала прочитай `user/gles2-renderer/README.md` и `results/02.md`, `04.md`–`08.md`.

## Что сделать

1. **`render-gles2-shell` реализует контракт целиком**: кадр (acquire = подготовка back-buffer-а, render, swap), повтор UI, live-настройки цвета/HDR (`RenderLiveSettingsAdapter`, без пересоздания программ на изменение ручки), диагностика, `wait_for_gpu_idle` (`glFinish` с таймаутом через fence), потеря контекста → существующий путь device lost. Conformance-набор из 01 зелёный на GLES2.
2. **Каталог Linux**: `[WgpuVulkan, Gles2]`. Пробы:
   - wgpu: есть адаптер Vulkan, `DeviceType != Cpu` (lavapipe/llvmpipe = `SoftwareAdapterOnly`) и нужные нам возможности есть. Проба лёгкая: экземпляр + перечисление адаптеров, без создания устройства и окна;
   - GLES2: libEGL грузится, контекст GLES2 создаётся, базовый уровень README выполнен.
3. **Метка незавершённого старта** (решение владельца 4):
   - файл в state-каталоге приложения пишется атомарно (`atomic-file-store`) перед созданием рендера семейства X и удаляется после **первого показанного кадра**;
   - если при старте метка X есть и профиль `Auto` → X пропускается, выбирается следующий кандидат, пользователь видит уведомление (через владельца уведомлений `mem:app-egui/notifications`). Текст — английский (`mem:app-egui/user-facing-language`): что случилось и как вернуть;
   - явный профиль (`Vulkan`) метку не обходит: пользователь выбрал сам. Это поведение для пользователя — подтвердить с владельцем;
   - в `Auto` при ошибке создания wgpu — сразу следующий кандидат в том же запуске (без перезапуска), уведомление.
4. **Настройки и конфиг.**
   - `render.profile`: значения `auto | vulkan | opengles` уже есть. Значение по умолчанию `vulkan` → `auto` (миграция схемы, как прошлые: `mem:config/schema-store-decomposition-s23`).
   - `[render.opengles]` `enabled`/`simple_ui` — удалить миграцией: `simple_ui` противоречит паритету (решение 7), `enabled` дублирует профиль. Добавить только нужное GLES2 (например swap interval) — по факту сессий 03–08, без заглушек «на будущее».
   - Смена профиля → новый механизм применения «после перезапуска»: в `settings-core`/`fastiplayer-settings` сейчас его нет (`AppRouteApplyResult` без restart-исхода, разведка). Строка в окне настроек «Applies after restart» + кнопка «Restart now» (перезапуск через существующий путь single-instance, с сохранением очереди/позиции, если это уже умеет resume). Если кнопка требует заметной работы — стоп, вопрос владельцу о минимальном варианте.
   - Пересоздание внутри семейства (Vulkan present mode и т.п.) работает как сейчас.
5. **Выбор маршрута декода** не меняется: `video_pipeline_selector` пересекает выходы бэкендов с возможностями **активного** рендера. На T410 для H.264 → VA-API + GLES2 zero-copy, для HEVC/VP9/AV1 → FFmpeg + GLES2 upload. Режим `dynamic` и предупреждение на GLES2 (решения 13–14) — в сессии 09b. Здесь только оставить место: семейство активного рендера должно быть доступно владельцу настроек и уведомлений через контракт, а не через тип рендера.
   - Факт 00: с установленным lavapipe текущий плеер на T410 стартует на процессорном Vulkan и тратит 216–280 % CPU. Проба `DeviceType::Cpu` → `SoftwareAdapterOnly` → GLES2 обязательна и проверяется на T410 с поставленным на время `vulkan-swrast`.
6. **Телеметрия/инфо**: в панели телеметрии и в «About/Diagnostics» видно активный рендер и причину выбора (`Auto: no hardware Vulkan adapter`).

## Поправки по итогам сессии 02 (`results/02.md`)

- Каталог уже есть: `app-egui::render_backend` (`selection.rs` — чистая `select_renderer` с таблицей тестов, `wgpu_vulkan.rs` — фабрика и единственный маппинг `[render.vulkan]`, `mod.rs` — `create_renderer` / `create_startup_renderer`). GLES2 добавляется строкой в `platform_candidates()` и своим модулем-фабрикой. Проба wgpu сейчас всегда «годен»: проверку аппаратного Vulkan и lavapipe (`DeviceType::Cpu`) добавляет эта сессия.
- Метка незавершённого старта: тип `RendererStartupMarker` и его ветки в `select_renderer` уже закреплены тестами; запись и чтение метки — здесь.
- Профиль `opengles` до этой сессии стартует wgpu с предупреждением в лог (`RequestedFamilyNotBuilt`); Apply в настройках с `opengles` — прежний `UnsupportedProfile` в `renderer_recreation.rs`. Обе ветки меняются здесь.
- Решение 5 («смена семейства только после перезапуска») уже соблюдается: пересоздание (`LiveRendererRecreation`) создаёт рендер того же семейства (`renderer.family()`).

## Поправки по итогам сессии 02a (`results/02a.md`)

- Каталог: запись GLES2 даёт маппинг настроек показа (как `wgpu_vulkan::surface_present_settings` для `[render.vulkan]`) для `render_backend::presentation_settings`. Классификатор `classify_renderer_settings_change` (`app-egui/src/renderer_recreation/settings_change.rs`) сравнивает группы целиком. Сейчас любое изменение `[render.opengles]` — полное пересоздание, любое изменение `[render.vulkan]` — перенастройка на месте. Если в группу добавляется поле, привязанное к устройству, или поле показа GLES2 (vsync), классификатор надо уточнить по полям. Компилятор ловит только новое поле верхнего уровня.
- Пересоздание внутри семейства GLES2 идёт той же транзакцией «отпустить окно → кандидат → …». Полный набор текстур новому рендеру даёт `UiTextureSync` — отдельной работы не нужно.

## Наследство сессии 03 (`results/03.md`)

- Маппинг показа в адаптере: `ShellPresentMode::{Auto, Fifo}` → `VsyncMode::WaitForVblank`, `Immediate` → `VsyncMode::Immediate`. `Mailbox` в EGL нет — `PresentationReconfigured::PresentModeFallback`. `max_frame_latency` у EGL не управляется — явный no-op-исход. `SurfaceAlphaPreference` → `WindowAlphaRequest`, смена альфы — `PresentationError::AlphaChangeRequiresRecreation` (у wgpu — `AlphaPreferenceChangeUnsupported`).
- `detach_presentation` работает и у потерянного контекста: окно освобождается для нового рендера после сброса GPU.
- Потеря: `Gles2Error::ContextLost` / `ContextLoss` → `RendererDeviceLost` + `release_after_device_lost` (через `GpuCompletionQueue::drain_after_context_lost`).
- Проба каталога: `CreateError::{EglUnavailable, EglVersionUnsupported, PlatformExtensionMissing, NoGles2Config, …}` — причины «не годен» без запуска рендера целиком.
- Тайминги `Gles2FrameTimings { surface_acquire, surface_present }` ложатся в одноимённые поля `RenderFrameStageTimings`.

## Наследство сессии 04 (`results/04.md`)

- Сборщик кадра `gles2_egui::FrameCompositor` уже повторяет стадии `EguiCompositor`. Адаптер `RenderShell` переводит их один к одному:
  - `UiContent::{Fresh, ReusePrevious}` ← `EguiFrameContent`;
  - `UiFrameOutcome::Dropped(RetainedUiUnavailable | UiTexturesOutOfSync | UiTexturesNotApplied)` → `Dropped(RetainedUiUnavailable)` / `Dropped(UiTexturesOutOfSync)` (последние два — оба в `UiTexturesOutOfSync`);
  - `Skipped(PresentationDetached | ZeroSize)` → свои исходы;
  - `can_reuse_retained_ui` → `FrameCompositor::can_reuse_retained_ui`;
  - `WindowCornerMask` → `CornerRadius::from_points`;
  - тайминги `UiFrameTimings` названы как `RenderFrameStageTimings`.
- Билет `AcquiredSurfaceFrame` у GLES2 — это `begin_frame` внутри `render`. Если адаптеру нужен отдельный acquire (контракт 06a), разделить `FrameCompositor::render` на «принять содержимое» и «нарисовать в начатом кадре», не меняя порядок стадий.
- Художнику передавать в egui `RawInput::max_texture_side = EguiPainter::max_texture_side()`. Текстура больше `GL_MAX_TEXTURE_SIZE` пропускается со счётчиком.
- `FrameCompositor::destroy(&context)` — до разрушения контекста (иначе в лог уходит предупреждение об утечке до смерти контекста).

## Наследство сессии 05 (`results/05.md`)

- Видеослой: `gles2_video::VideoLayerRenderer`. Порядок: `upload` до кадра → `VideoUnderlay` в `UiFrameRequest.layers` → после `FrameCompositor::render` один fence кадра, `Arc<GpuFence>`. Этот fence отдаётся `retire_submitted_work` и им же освобождаются кадры декодера.
- **Решение 30:** кадр декодера возвращается в пул только после fence кадра показа, в котором GPU его последний раз трогал, — для любого вида кадра. Обёртка освобождения строится по образцу `WgpuSubmittedResourceProvider` (`release_frame` откладывается до fence, ровно один раз; при потере контекста — сразу).
- `Busy` кольца — как `Busy` пула wgpu: прошлый кадр остаётся, кадр декодера не освобождается, тот же кадр — на следующем показе. После смены декодера (ключи кадров начнутся заново) — `forget_uploaded_frames`.
- Перевод кадра — `render_gles2_shell::host_planar_frame_upload`; где и как рисовать — `VideoDrawPlan::for_frame` (тот же `render-core`, что у wgpu); возможности — `gles2_render_capabilities`.
- **Пропускать заливку фона `FrameCompositor`, когда видео накрывает цель:** видеослой сам заливает чёрным всё, куда не рисует. На T410 полная заливка — 2,2 мс GPU, а бюджет 1080p60 у края (проход 11,8 + загрузка ~1,8 + фон 2,2 ≈ 16 мс).
- Стенд замера с настоящим FFmpeg без плеера — `video-ffmpeg/examples/gles2_upload_bench.rs`.


## Наследство сессии 06 (`results/06.md`)

- `VideoDrawPlan::for_frame` принимает `&RenderLiveSettings`, то есть тот же снимок цвета и HDR→SDR, что wgpu. Живые настройки GLES2-шелла идут через `RenderLiveSettingsAdapter`.
- Возможности — `gles2_render_capabilities(caps)`: при `highp` объявлены 10/12 бит и BT.2446-C. HDR программной загрузки проверяется `RenderCapabilities::supports_hdr_to_sdr_for_contract` (P010 не нужен).
- **Решение владельца до кода:** где строить таблицу HDR. Сейчас она строится в `VideoLayerRenderer::draw` при смене «перенос + уровни». На T410 это 41–50 мс CPU разово, при перетаскивании ползунка уровней — на каждом шаге. Варианты:
  - строить в фоне (`HdrLutTable::build` — чистая функция без GL) и держать старую таблицу до готовности;
  - строить только на «Применить».
- Ошибки плана 10/12 бит (`VideoDrawPlanError::HighBitColor`) — ошибки рендера, как у wgpu. Тихой подмены нет.

## Наследство сессии 07 (`results/07.md`)

- Обёртка освобождения: `render_gles2_shell::{Gles2FrameReleaseBinding, wrap_video_backend_for_gles2_release}` (решения 30 и 32).
  - Подготовщик импорта оборачивает backend в потоке плеера, без GL-объектов.
  - Рендер после КАЖДОГО кадра показа зовёт `publish_presented_frame_fence(Arc<GpuFence>)` — тот же `Arc`, что `VideoLayerRenderer::retire_submitted_work`.
  - При потере контекста — `release_after_context_lost`. При пересоздании — `resume_after_context_recreated`.
  - **Перед уничтожением `Gles2Context` рендер обязан вызвать `renderer_detaching(RendererDetachGrace::DEFAULT)`.** Без этого на T410 процесс падает при выходе (дефект 2 отчёта 07).
- HW-импорт: `render_gles2_shell::materialize_dma_buf_frame(renderer, context, provider, frame)`.
  - Исходы отображаются в `FrameMaterializationOutcome` по таблице в `dma_buf_frame/outcome.rs`. `Unsupported(..)` → `Unsupported(DmaBufDescriptorRejected)`: это запускает существующий программный фолбэк.
  - По плану импорта — `set_dma_buf_import_capacity(dma_buf_import_capacity_for(plan.decoder_surface_pool))`.
  - При смене декодера — `purge_dma_buf_imports`: текстуры держат память старых поверхностей.
  - Счётчики кэша — `dma_buf_import_snapshot()` в `FrameImportStats`. Поле трассы `dma_buf_vulkan_imports` переиспользуется под импорты EGL.
- Возможности: `gles2_render_capabilities` объявляет NV12 DMA-BUF (composed и separate) только при уровне `zero_copy`. P010 не объявляется (решение 33).
- Поток VA-API декодера не присоединяется при уничтожении (общий бэклог, п. 13). **Проверка обязательна (решение 34):** серия выходов приложения на T410 во время воспроизведения HW-видео, в обоих режимах окна. Падение → причина и явное завершение потока декодера до выхода.

## Наследство сессии 08 (`results/08.md`)

- Субтитры: `render_gles2_shell::{Gles2SubtitleOverlayRenderer, SubtitleOverlayLayer}`.
  - Видео и субтитры в одном кадре — `gles2_egui::FrameLayerStack::new(&mut video_underlay, &mut subtitle_layer)`.
  - Видимая область — `clamp_to_surface` области видео, как `clamp_video_viewport_to_screen` у wgpu.
  - Отказ загрузки атласа не роняет кадр: исход `SkippedUploadFailed` и счётчик `failed_uploads`.
  - Ошибка кадра — только чужой контекст или отказ рисования.
  - `destroy` слоя — до уничтожения контекста, как у видеослоя.
- Диагностика:
  - `render_gles2_shell::render_frame_timing(&UiFrameTimings, renderer_elapsed)` строит `RenderFrameOutcome::Presented`. `renderer_elapsed` — всё время кадра рендера, вместе с `surface_acquire`, как у wgpu.
  - Исход строится только из `UiFrameOutcome::Presented`: его сборщик возвращает после успешного `eglSwapBuffers`, а по нему приложение пишет `video_render_acceptance`.
  - `RenderShell::diagnostics` — `video_pass_diagnostics(&RenderableFrame, &VideoDrawPlan)` последнего видеопрохода; кадр без видео — `RenderDiagnostics::default()`.
  - `import_stats` — `frame_import_stats(&renderer.dma_buf_import_snapshot())`.
  - `hot_path_stats` — нули (`RenderHotPathStats::default()`): групп привязки у GLES2 нет, а записи uniform-ов видеослой пока не считает. Если 10–11 понадобятся — счётчик у видеослоя, не в шелле.
- **Имя контракта VA-API декодера под GLES2 — обязательно.**
  - Сейчас `video-vaapi` всегда пишет `backend_contract="VA-API Vulkan DMA-BUF"` (`ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf`).
  - Под GLES2 он обязан писать `VA-API EGL DMA-BUF`: рендер передаёт свой контракт импорта при старте декодера.
  - `evidence.py` (сессия 08) доказывает аппаратный путь GLES2 только по `plan="vaapi-dmabuf-gles2"` вместе с `VA-API EGL DMA-BUF`. Следы двух семейств в одной попытке — не доказательство: прогоны 10–11 упадут на `backend_confirmed`, пока имя не исправлено.

## Тесты (функциональные, результат, а не вызов)

- Видео из фикстуры через GLES2 доходит до показа: headless EGL + реальный FFmpeg-декодер → `RenderFrameOutcome::Presented` с ожидаемым pts и пикселями кадра (не «декодер открылся»). Для HW — `#[ignore]` на машине с VA-API.
- Seek, resize, смена файла, неподдерживаемый формат (типизированная ошибка, а не чёрный экран), потеря контекста (fake) → освобождение кадров и восстановление.
- `select_renderer`: все комбинации профиля × проб × метки; lavapipe-only → GLES2; метка X в `Auto` → следующий кандидат + уведомление; явный профиль + метка → поведение по решению владельца.
- Миграция конфига: старый `vulkan` по умолчанию → `auto`; явно выбранный пользователем `vulkan` остаётся `vulkan`. Если в старом конфиге их не различить — решение владельцу. `simple_ui` удалён без потери остальных полей.
- Настройка: смена профиля не пересоздаёт рендер вживую, а помечает «после перезапуска».

## Ручная приёмка

- Основная машина: `profile = opengles` → весь UX как в wgpu (seek/scrub/pause/resume, смена файла, плейлист, фуллскрин и автоскрытие, сайдбар-исключение, субтитры обоих видов, скругление углов, цветокоррекция и HDR live-preview).
- Отключить Vulkan-ICD (`VK_ICD_FILENAMES` на несуществующий) → `Auto` выбирает GLES2 с уведомлением. Только lavapipe → GLES2.
- T410: первый настоящий запуск Fastiplayer.
- Всё перечисленное — в обоих режимах, нативный Wayland и XWayland, на обеих машинах (решение владельца).

## Критерий выхода

- Плеер стартует и играет на T410 в `Auto` без ручных настроек.
- Выход приложения на T410 во время HW-воспроизведения чистый во всей серии (решение 34, бэклог п. 13 закрыт или исправлен).
- На основной машине `Auto` = wgpu, поведение без изменений (короткий замер «до/после», как в 02).
- Memories: новая `mem:gles2-renderer/core` дополнена, `mem:render-video/core`, `mem:config/...`, `mem:settings-ui/...` обновлены.

Memories: `mem:app-egui/startup-orchestration-s17`, `mem:app-egui/notifications`, `mem:settings-ui/application-contract-s08`, `mem:settings-ui/multiframe-apply-c2-2026-10-11`, `mem:config/schema-store-decomposition-s23`, `mem:app-egui/instance-forwarding-ux13`.
