# Сессия 03: кэш Vulkan-импорта по стабильной идентичности и явная синхронизация

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 03 плана `user/cpu-parity/`: сделай так, чтобы DMA-BUF каждой поверхности пула импортировался в Vulkan один раз, а не на каждом кадре, с явной и доказанной синхронизацией VA-writer → Vulkan-sampler при повторном использовании поверхности. Требует завершённой сессии 01. Сначала прочитай `user/cpu-parity/README.md`, `results/00.md`, `results/01.md`.

## Проблема

`DmaBufWgpuTextureCache` (`crates/render-wgpu-video/src/video/dma_buf_materializer.rs:173-232`) ключуется по `FrameResourceHandle`, уникальному на кадр → промах на каждом новом кадре → `import_exported_dma_buf_image` (`dma_buf_import.rs:528-870`): dup fd, `vkCreateImage`, `vkGetImageMemoryRequirements`, `vkAllocateMemory` с import, `vkBindImageMemory`, `create_texture_from_hal`, два `create_view`; через 24 кадра — уничтожение. Контракт `ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf()` (`video-vaapi/src/zero_copy_surface_pool.rs:39-47`) держит `explicit_external_memory_reuse_sync: false` именно потому, что импорт-слой не оформляет переходы владения между VA и Vulkan.

Дополнительно на каждую выборку, включая повторы кадра: `try_resource_descriptor_lookup` → `try_clone_for_lookup` (`video-core/src/resource/descriptor.rs:389-393`) делает `dup()`+`close()` fd ещё до проверки кэша; декодер делает `fstat` дважды (`resource_pool.rs:~296`).

## Архитектура (решения — после чтения кода и Context7 по wgpu-hal/ash/Vulkan)

- **Идентичность для кэша:** стабильный ключ поверхности пула — `VASurfaceID` + `st_dev/st_ino` объектов DMA-BUF + layout/modifier/размеры (уже есть в `ZeroCopySurfaceDescriptor`). Не fd-номер. Ключ формирует владелец дескриптора (`video-vaapi` → `video-core` нейтральный `FrameResourceIdentity`/подобный тип), рендерер его только сравнивает. `FrameResourceHandle` остаётся уникальным на кадр и живёт поверх.
- **Владелец кэша:** `render-wgpu-video` (`dma_buf_materializer.rs`), ёмкость = ёмкость пула поверхностей (сегодня 24), не больше. Кэш должен инвалидироваться по сигналу владельца пула: смена формата/разрешения, пересоздание пула, `invalidate_all`, переключение бэкенда (`mem:render-video/controlled-renderer-recreation-s08c`).
- **Синхронизация при переиспользовании импортированного image.** Сегодня каждый импорт стартует из `initialLayout = UNDEFINED`/`TextureUses::UNINITIALIZED`, и wgpu сам ставит первый барьер. При переиспользовании wgpu-трекер считает image уже в `SHADER_READ`, барьера не будет, а память в это время писал VCN. Нужно явно: (1) CPU-сторона — `vaSyncSurface` до публикации (есть); (2) GPU-сторона — перед первым сэмплированием нового содержимого acquire-барьер по памяти (release из `VK_QUEUE_FAMILY_FOREIGN_EXT`/external → graphics queue, `srcAccess = MEMORY_WRITE`, `dstAccess = SHADER_READ`, инвалидировать кэши) через `CommandEncoder::as_hal`/ash на hal command buffer, либо через wgpu-hal `transition_textures` с принудительным состоянием, если wgpu 30 это позволяет; (3) перед возвратом поверхности VA — GPU завершил чтение (есть: `on_submitted_work_done`), плюс при необходимости release-барьер в foreign family. Документировать в коде, почему достаточно, со ссылками на VUID/спецификацию. Если на данном стеке wgpu/ash нет законного способа выдать барьер — **стоп: вопрос владельцу**, не «просто кэшировать и посмотреть».
- **Контракт:** `explicit_external_memory_reuse_sync` переводится в `true` только когда рендерер фактически выполняет барьер; `allows_persistent_reuse()` начинает возвращать `true`, `begin_import_or_reuse` даёт `Reuse` для свободного слота с той же идентичностью. Счётчики `imports_reused/replaced/created` приобретают реальный смысл.
- **Дешёвый повтор кадра:** проверять кэш **до** dup дескриптора; дескриптор дублировать только на промахе. `fstat` — один раз на экспорт.

Memories: `mem:render-video/core` (особенно раздел про `initialLayout = UNDEFINED`, format list и известное ограничение foreign-queue acquire), `mem:render-video/dma-buf-frame-contract-aud016-2026-08-23`, `mem:video-vaapi/core`.

## Инварианты

1. Рендерер никогда не держит fd/дескриптор, который декодер может закрыть под ним (причина нынешнего dup на lookup) — при кэшировании владение image должно переживать замену слота.
2. Без доказанной синхронизации переиспользование запрещено — fail-closed, как сегодня.
3. Кэш ограничен ёмкостью пула, не растёт; при `FormatChanged`/reconfigure старые image уничтожаются до создания новых (RSS/GTT не растут).
4. `Busy`/`Missing`/`Fatal`/`Unsupported` семантика материализатора сохраняется.
5. Vulkan validation layer (`VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`) на релизном бинарнике — чисто, как после обновления wgpu 30 (кроме известного `VUID-StandaloneSpirv-None-10684`).

## Тесты

- Hermetic: кэш по идентичности — тот же surface id + inode → hit без `import`, другой handle тот же буфер → hit, тот же surface id после инвалидации → miss; ёмкость не превышается; fake-importer считает вызовы. Должен падать на текущем коде (сегодня новый handle = miss).
- GPU `#[ignore]` на базе `dma_buf_import/udmabuf_render_tests.rs`: один и тот же udmabuf перезаписывается CPU между кадрами → после барьера рендер читает новое содержимое (readback), без барьера тест документирует, что проверяет именно это.
- Аппаратный `#[ignore]` прогон: за N кадров число Vulkan-импортов ≤ ёмкости пула (счётчик материализатора в диагностике).

## Замер и критерий выхода

- До/после `run_pass.py` HW H.264 1080p60, HEVC 4K60, 4K30; `agg_timing.py`: `video_prepare` на 4K и 1080p сравнимы и ≪ 0,25 мс.
- Validation layer чист; quality-gate README (особенно seek/scrub, смена файла с другим разрешением, hardware↔software, 10-бит P010, HDR→SDR).
- Отчёт `results/03.md`, обновить `mem:render-video/core` (кэш, барьер, контракт) и `mem:video-vaapi/core` (контракт `allows_persistent_reuse`).
