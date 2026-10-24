# Сессия 07: аппаратный zero-copy — DMA-BUF → EGLImage

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 07 плана `user/gles2-renderer/`: реализуй в `gles2-video` импорт DMA-BUF кадров VA-API в EGLImage без копий (раздельные плоскости R8/GR88, модификаторы), кэш импорта по идентичности поверхности, освобождение кадра декодера только по EGL fence, P010 там, где драйвер позволяет. Проверь на 780M и на T410. Сначала прочитай `user/gles2-renderer/README.md`, `results/00.md` (что доказал стенд) и `results/05.md`.

## Зачем

На T410 аппаратно декодируется только H.264. Зато это самый массовый кодек, и именно на нём соперники сравнимы. Инвариант README: строго zero-copy, иначе контракт не объявляется.

## Что доказал стенд 00 (`results/00.md`)

- T410: поверхности VA **линейные** (модификатор `0x0`), pitch = ширине, поверхность 1920×1088 для кадра 1080. COMPOSED (1 объект, NV12, 2 плоскости) и SEPARATE (R8 + GR88) импортируются оба. Readback бит-в-бит совпадает с программным декодом.
- 780M: модификатор tiled AMD. **Без атрибутов модификатора хрома — тихий мусор** (импорт «успешен»). Модификатор передавать всегда, когда он не `INVALID`. P010 через GR88 + ABGR8888 — бит-в-бит.
- `eglCreateImage` + destroy двух плоскостей — 0,006–0,007 мс. Кэш по identity нужен не ради скорости импорта, а ради `dup`/`fstat` и стабильных текстур; решение — по замеру сессии.
- Выборка из импортированной линейной поверхности на Ironlake не медленнее обычной текстуры: проход NV12→RGB 1440×900 — 5,6 мс.
- **Ограничение T410: декодер занимает то же 3D-ядро.** На 1080p30 после декода у GPU остаётся ~8 мс на кадр, на 720p60 ~6 мс. Проход должен быть один, без промежуточных целей. Живой прогон HW 1080p60 на T410 невозможен (потолок кремния ~39,6 кадра/с), а 1080p30 упирается в наш декодер до сессии 09a.

## Что сделать

1. **Импорт** (`gles2-video::dma_buf`, `unsafe` в `dma_buf/safety.rs`):
   - **Основной путь — раздельные плоскости**: Y как `DRM_FORMAT_R8`, UV как `DRM_FORMAT_GR88` (offset/pitch/modifier из `DmaBufFrameDescriptor.layers`), два `EGLImage` → `glEGLImageTargetTexture2DOES` → обычный `sampler2D`.
     - Цвет считает наш шейдер из 05: паритет цветокоррекции и матриц.
     - `samplerExternalOES` (драйвер сам переводит в RGB) не используем: он ломает паритет матриц, диапазона и цветокоррекции.
   - Composed-экспорт NV12 (один объект, два слоя) импортируется тем же способом по плоскостям. Порядок попыток экспорта уже есть в `cros-codecs-patch` (NV12 composed-first). Если драйвер не принимает R8/GR88 из composed-описания, запросить separate-layers — решение по факту из `results/00.md`.
   - Модификатор передаётся только при `EGL_EXT_image_dma_buf_import_modifiers` и не `DRM_FORMAT_MOD_INVALID`. `DRM_FORMAT_MOD_LINEAR` / `NONE` на Ironlake (ожидание из разведки) — проверить.
   - fd: дубликат CLOEXEC `OwnedFd`, после `eglCreateImage` закрывается нами (EGL держит свою ссылку на dma-buf). Тесты владения — по образцу `render-wgpu-video/src/dma_buf_import/safety.rs`.
2. **Кэш импорта по `DmaBufFrameIdentity`** (урок cpu-parity 03: ключ по `FrameResourceHandle` = промах на каждом кадре):
   - generic LRU, ёмкость = пул поверхностей декодера (`DmaBufImportCacheCapacity::matching_surface_pool` — переиспользовать или повторить в адаптере);
   - смена format signature уничтожает старые записи до импорта; тот же resource_id с другой identity заменяет запись;
   - на попадании всё равно валидация identity против контракта кадра;
   - `try_resource_identity_lookup` (без dup fd) → кэш → `descriptor_lookup` только на промахе.
3. **Синхронизация «VA-писатель → GL-читатель».** В GL через EGLImage она неявная (dma-fence ядра), и декодер делает `vaSyncSurface` до передачи кадра. Записать это как явный контракт: аналог `ZeroCopyImportReuseContract` для GLES2. Проверка — тест «поверхность перезаписана между кадрами → видно новое содержимое без переимпорта» (как udmabuf-тест cpu-parity 03).
4. **Освобождение после GPU** — `bind_video_backend` контракта для GLES2:
   - fence после кадра, который читал поверхность; освобождение у provider-а ровно один раз (CAS, как `PendingSubmittedRelease::release_once`);
   - **не зависит от следующего кадра UI** (рендер по изменению может спать): поток-ожидатель fence-ов или пробуждение по таймеру, — решить на стенде и описать владельцу, если это меняет потоки приложения;
   - потеря контекста → освобождение всех ожидающих (как `release_after_device_lost`);
   - обёртка `VideoDecoderThreadHandle` пробрасывает **все** методы. Урок двух прошлых багов: `decoder_activity_snapshot`, `host_upload_resource_snapshot`. Тест, который ловит забытый проброс (перечень методов трейта).
5. **P010 / 10-бит HW** (только где есть; на Ironlake не нужен — там нет 10-бит декода):
   - Y как `GR88` (2 байта), UV как `ABGR8888` (2×16 бит), сборка в шейдере из 06, nearest.
   - Если драйвер не импортирует — контракт не объявляется. Это единственный разрешённый владельцем компромисс.
6. **`RenderCapabilities` GLES2**: контракт `HardwareZeroCopy { DmaBuf { image_layout } }` объявляется только при подтверждённой цепочке (возможности 03 + пробный импорт на старте, если без него никак). `capability-core` получает только возможности **активного** рендера (разведка: объединение двух рендеров ломает выбор).

## Поправки по итогам сессии 02 (`results/02.md`)

- В контракте появился `RenderShell::video_import_preparer() -> Box<dyn VideoImportPreparer>` (решение владельца в сессии 02). Кандидат нового файла запускается в потоке плеера, поэтому подготовка импорта (обёртка provider-а с освобождением по EGL fence) обязана работать вне потока GL: подготовщик не создаёт GL/EGL-объектов, EGLImage делаются в `materialize` на потоке рендера. Conformance-проверка №7 закрепляет это для каждого рендера.
- Образец склада — `render-wgpu-shell/src/video_import.rs`: `PendingImportOrigin::{FreshDecoder, RecreatedRenderer}` (перепривязка очереди освобождений только для пересоздания), проверка «контракт объявлен» первой в `materialize`.

## Наследство сессии 03 (`results/03.md`)

- Освобождение по fence:
  - `Gles2Context::insert_fence()` → `GpuFence` (`Send + Sync`, ждать можно из потока плеера);
  - `GpuCompletionQueue<T>`: `drain_completed()` по порядку и ровно один раз, `drain_after_context_lost()` при потере;
  - без `EGL_KHR_fence_sync` — `glFinish` и счётчик `FenceStats::fence_fallback_finishes`.
- Форматы и модификаторы импорта: `capabilities().dma_buf()` (`DmaBufImportSupport::Queried`, `FormatSampling::{Sampler2D, ExternalOnly, ImplicitModifierOnly}`). Уровни — `tiers().zero_copy` / `zero_copy_10bit`.
- Живой вывод 03: на T410 R8/GR88/ABGR8888 — `LINEAR` и `X_TILED` (`0x0100000000000001`) как `sampler2D`, NV12/P010 только external.
- Функции `eglCreateImage`/`glEGLImageTargetTexture2DOES` — в свой модуль безопасности (по образцу `context/safety.rs`).

## Наследство сессии 05 (`results/05.md`)

- Тип источника уже есть: `gles2_video::PlaneSource::DmaBuf(DmaBufPlane { fd, offset, pitch, modifier, drm_format })`, сейчас — `VideoUploadError::PlaneSourceUnsupported`.
- Кольцо слотов, fence и `retire_submitted_work(Arc<GpuFence>)` готовы. Для импорта EGLImage слот — это кэш импорта, а охрана та же: кадр держится до fence последнего кадра показа (решение 30).
- Шейдер и uniform-ы цвета, letterbox, поворот и исключения переиспользуются как есть (`program.rs`, `draw_uniforms.rs`). Для NV12 (UV в одной текстуре) нужен свой вариант программы с той же схемой координат.


## Тесты

- `#[ignore]` GPU: udmabuf (memfd → `/dev/udmabuf`) NV12 с известными цветами → production-импорт → readback; перезапись буфера между кадрами видна без переимпорта (порт `udmabuf_*` тестов wgpu).
- Кэш: N поверхностей пула → N импортов за прогон, остальное попадания; смена формата → полный сброс; счётчики `dma_buf_*` в трассе.
- Освобождение: кадр освобождается только после fence; ровно один раз; при потере контекста — все сразу; при спящем UI — всё равно освобождается (тест с fake-fence).
- Живой прогон без копий (счётчик импортов = размер пула, `perf` без memcpy кадра): HW H.264 1080p60 на 780M (GLES2); на T410 — H.264 720p30 и 720p60, 1080p30 — после 09a.

## Замер

HW H.264 1080p60 и 720p60: GLES2 против wgpu на 780M. На T410 — первый живой замер 720p30/720p60 против эталона из `results/00.md`: у mpv/VLC HW 720p30 — 0 потерь за ~12 % CPU, 720p60 — 328–405 потерь. Официальная квалификация — в 11.

## Критерий выхода

- Zero-copy доказан счётчиками и профилем на обеих машинах.
- Освобождение покрыто тестами, decoder pool не истощается на паузе и при спящем UI.
- Ограничения драйвера (модификаторы, форматы) записаны в таблицу совместимости для 13.

Memories: `mem:video-vaapi/core`, `mem:render-video/dma-buf-frame-contract-aud016-2026-08-23`, `mem:render-video/dma-buf-layout-fallback-s15`, `mem:render-video/core` (кэш импорта cpu-parity 03), `mem:video-ffmpeg/ffi-safety-contracts`.
