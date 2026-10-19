# Сессия 01: настоящий пул VA-поверхностей

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 01 плана `user/cpu-parity/`: замени «пул дескрипторов» в `video-vaapi` на настоящий ограниченный пул VA-поверхностей, чтобы в установившемся воспроизведении не было `vaCreateSurfaces`/`vaDestroySurfaces` на каждый кадр. Сначала прочитай `user/cpu-parity/README.md` (решения владельца, общие правила, quality-gate) и `user/cpu-parity/results/00.md`. Замер «до/после» обязателен.

## Проблема

`DmaFramePool` (`crates/video-vaapi/src/frame_pool.rs`) хранит только `InternalVaapiFrame { resolution, rt_format }` — Copy-дескриптор без поверхности. Поверхность создаётся в `InternalVaapiFrame::to_native_handle` (`internal_vaapi_frame.rs:92-116`) через `display.create_surfaces(...)` при каждом `VaapiPicture::new` в патче cros-codecs (`crates/cros-codecs-patch/src/backend/vaapi/decoder.rs`), и уничтожается в `Drop for Surface` (`crates/cros-libva-patch/src/surface.rs:436-441`), когда плеер отпускает `DecodedHandle` после release рендерера (`decoder.rs:~495 release_zero_copy_frame` → `suppressed_reclaim.rs:171-184`). `alloc_or_allocate` никогда не возвращает «нет буфера», поэтому путь `NotEnoughOutputBuffers` в адаптерах практически недостижим.

Следствия: аллокация и освобождение GPU-буфера в ядре на каждый кадр (стоимость растёт с разрешением), новый DMA-BUF и новый GEM-объект при каждом экспорте, поэтому и Vulkan-импорт на стороне рендерера не может дедуплицироваться ядром.

## Архитектура (описать в начале сессии, уточнить по коду)

- **Владелец состояния:** `video-vaapi` (пул поверхностей — рядом с `DmaFramePool`, его и заменяет; отдельный модуль, например `surface_pool.rs`, если файл растёт). Патч cros-codecs меняется минимально: `VaapiPicture::new` получает поверхность из пула через существующий alloc-callback, а не создаёт её.
- **Контракт пула:** фиксированная ёмкость из `VaapiDecoderRuntimeConfig.surface_pool_frames` (сегодня 24; размер должен покрывать DPB кодека + in-flight + present queue + render leases — проверить `approximate_reserved_surface_headroom` учёт). Пустой пул → адаптер получает типизированный `NotEnoughOutputBuffers`/backpressure, **а не** новую поверхность. Поверхность возвращается в пул, когда её отпустили **и** декодер (DPB/handle), **и** рендерер (слот `Free` в `ZeroCopySurfacePool`: порядок `WaitingGpuCompletion → WaitingDecoderReuse → Free` сохраняется).
- **Идентичность:** `VASurfaceID` теперь стабилен и повторяется между кадрами — это ожидаемо и нужно сессии 03. `FrameResourceHandle` остаётся уникальным на каждый кадр (seek/generation/release-учёт ключуются по нему). `ZeroCopySurfacePool::begin_import_or_reuse` для известного `surface_id` со свободным слотом должен давать `Reuse` только при `allows_persistent_reuse()`; до сессии 03 контракт остаётся `false`, значит путь `Replace` — это нормально, но убедись, что `Replace` не уничтожает поверхность (она принадлежит пулу, а не слоту).
- **Смена формата/разрешения (`FormatChanged`), flush, reconfigure, drop декодера:** пул пересоздаётся/очищается только после force-drain всех удержанных handle-ов (существующие инварианты `mem:video-vaapi/core`). Поверхности другого размера в пуле не переиспользуются.
- **Что не трогаем:** cros-codecs `VaSurfacePool`/`PooledVaSurface` энкодеров; экспорт DMA-BUF (`vaExportSurfaceHandle` раз на кадр остаётся — его убирает сессия 03 вместе с кэшем); `player-core`; рендерер.

Memories: `mem:video-vaapi/core`, `mem:dependency-patches/core`, `mem:panic-invariant-policy`, `mem:video-core/decoder-stream-boundary`.

## Инварианты

1. Рендерер никогда не читает поверхность до `vaSyncSurface` (сохраняем текущий `handle.sync()` перед экспортом).
2. VA не пишет в поверхность, пока рендерер её держит (слот не `Free`) или пока GPU не завершил чтение (`on_submitted_work_done`).
3. Пул ограничен: ни при каких условиях число живых поверхностей не превышает ёмкость; переполнение — backpressure, не аллокация и не паника.
4. Pts/generation/seek-preroll/suppressed-reclaim семантика не меняется.
5. `DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS` и `surface_pool_frames` не увеличиваются «чтобы хватило».

## Тесты

- Unit на пул: выдача до ёмкости, отказ при исчерпании, возврат → повторная выдача той же поверхности, очистка при смене формата; fake-поверхности без железа.
- Тест адаптера (fake backend): при исчерпании пула `submit_packet` возвращает output backpressure и сохраняет pending AU без двойного потребления (существующие H.264/H.265 инварианты).
- Аппаратный `#[ignore]` регресс (рядом с `tests/av1_mp4_seek_regression.rs`): после N кадров установившегося воспроизведения число созданных VA-поверхностей ≤ ёмкости пула (счётчик в пуле), seek и EOF проходят. Должен падать на текущем коде.
- Прогон существующего hardware regression и `cargo test -p video-vaapi -p cros-codecs`.

## Замер и критерий выхода

- До/после `run_pass.py`: `fp-hw-h264=fastiplayer:hw:h264`, `fp-hw-hevc=fastiplayer:hw:hevc`, `fp-hw-4k30=…` (см. `results/00.md`). Ожидание: `video-decode` поток и ядерная доля главного потока падают; `video_prepare` на 4K заметно ниже 0,7 мс; `imports_created` по-прежнему 60/с (это нормально до сессии 03), но `vaCreateSurfaces` в профиле отсутствует.
- Quality-gate из README: уникальные кадры, `drops_*`, seek/scrub/pause, смена файла, переключение hardware↔software, 10-битный HEVC (P010).
- `scripts/ci-checks.sh all`, `scripts/check-refactor-guardrails.py`.
- Отчёт `results/01.md`, обновить `mem:video-vaapi/core` (владелец пула, контракт исчерпания) и `mem:cpu-parity/core`.
