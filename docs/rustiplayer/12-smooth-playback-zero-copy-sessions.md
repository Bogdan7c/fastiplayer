# 13. Smooth Playback and Zero-Copy Sessions

Этот документ разбивает работу над идеально плавным воспроизведением на
самостоятельные сессии. Цель всех сессий одна: любые поддерживаемые видео должны
воспроизводиться стабильно, без late drops, скрытых CPU fallback-ов и
codec-specific хака в общих слоях. План сразу учитывает будущие codec/backend
комбинации, а не только текущий VP9/VA-API/wgpu путь.

## Как пользоваться документом

- В новом чате прикладывать этот файл целиком.
- Дополнительно вставлять только блок `Контекст для копипаста` нужной сессии.
- Работать строго по одной сессии за раз.
- Не переходить к следующей сессии, пока текущая не прошла self-review и ручные
  проверки.
- Если в коде найдено важное архитектурное расхождение с планом, остановиться и
  обсудить решение до правок.

## Общие правила для всех сессий

- Перед правками задавать рабочую директорию через MCP `code_index` и запускать
  deep index.
- Перед правками кода сверяться с Context7 по релевантным библиотекам, API и
  backend-ам.
- Искать причину проблемы, а не маскировать симптом.
- Не добавлять software video decode, CPU readback или CPU upload fallback.
- Не оставлять скрытых escape hatch-ей через env vars, config или diagnostic mode
  в production path.
- Не смешивать demux, decode, scheduling, render и UI в одном слое.
- Все новые runtime-настройки должны иметь config schema, defaults, validation и
  документацию.
- Любой новый codec/backend должен проходить через общие контракты capabilities,
  frame contract и zero-copy surface lifecycle.
- Ошибки должны быть typed и видимыми в snapshot/event/log diagnostics.
- Каждый этап должен оставлять проект в runnable/checkable состоянии.

## Целевые инварианты

- Поддерживаемое видео либо идет по hardware decode + zero-copy present path, либо
  получает typed reject до старта воспроизведения.
- `FrameMemoryPath::CpuUpload` не может попасть в production video path.
- `VideoExportPath::CpuReadback` не рекламируется как production capability.
- Renderer не принимает video frame, который нарушает zero-copy contract.
- Decode/render pipeline не импортирует и не уничтожает expensive GPU/external
  ресурсы без bounded pool и понятного lifecycle.
- Render thread не ждет worker на hot path дольше, чем требуется для lock-free или
  bounded non-blocking handoff.
- Worker умеет догонять burst-ы decode/demux после сложных сцен, а не ограничен
  жестким nominal 60Hz бюджетом.
- Diagnostics позволяют доказать, какой path использовался у каждого кадра:
  zero-copy, codec profile, surface format, sync latency, import latency, queue
  depth, drop reason.
- Общие слои не знают про VP9-специфику; VP9, AV1, HEVC и будущие codec-и
  подключаются через адаптеры и typed capability requirements.

## Приоритеты

1. Запретить CPU fallback на уровне архитектуры.
2. Добавить измерения, чтобы каждый drop имел причину.
3. Убрать per-frame zero-copy import/destruction churn.
4. Убрать блокирующие узкие места decode thread и backpressure.
5. Перестроить playback scheduler под 4k60+ burst/headroom.
6. Сделать render acquisition неблокирующим и стабильным.
7. Закрепить codec-neutral contracts для будущих форматов.
8. Добавить regression/stress проверки для плавности и zero-copy.

## Сессия 1. Zero-Copy Contract Lockdown

### Контекст для копипаста

Реализуй Session 1 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: сделать zero-copy обязательным production-инвариантом. Нельзя чинить
плавность через CPU upload/readback/software fallback. Сначала найди все текущие
точки CPU fallback, затем переведи production path в fail-fast/fail-closed режим
для всех video formats, включая NV12. В конце сделай self-review из этой сессии.

### Цель

Убрать возможность скрытого CPU video path. После этой сессии проект может
проигрывать меньше файлов, но не должен проигрывать ни один файл через CPU upload
или readback.

### План реализации

- Найти все production-достижимые упоминания:
  - `FrameMemoryPath::CpuUpload`;
  - `VideoExportPath::CpuReadback`;
  - `queue.write_texture` для video planes;
  - `map()`/VA image readback для decoded video;
  - env/config switches, которые отключают zero-copy.
- Разделить test/dev helpers и production path:
  - production build не должен иметь runtime-переключатель на CPU upload;
  - test fixtures могут использовать отдельный явно названный test-only path;
  - diagnostic path не должен включаться случайно через env.
- Сделать decoder fail-fast:
  - если DMA-BUF export недоступен, вернуть typed error;
  - если DMA-BUF import недоступен, вернуть typed error;
  - если импорт NV12/P010 не прошел, не fallback-ить в CPU upload.
- Сделать renderer fail-closed:
  - `NV12` и `P010` должны требовать zero-copy memory path;
  - нарушение контракта должно быть typed render/decode error.
- Обновить capability reporting:
  - не рекламировать CPU readback как поддерживаемый production export path;
  - явно показывать, что backend поддержан только при hardware decode +
    zero-copy export/import intersection.
- Обновить config/docs:
  - добавить формулировку `zero_copy_video_only = true` как архитектурный
    инвариант или documented non-configurable policy;
  - если нужен dev/test fallback, описать его как compile-time test feature, а не
    runtime option.

### Acceptance

- В production video path нет достижимого CPU upload/readback fallback.
- NV12 защищен так же строго, как P010.
- Невозможность zero-copy дает понятную typed ошибку до или во время старта
  playback, а не скрытое снижение качества pipeline.
- `cargo check` проходит.
- Релевантные unit tests обновлены или добавлены.
- Ручной запуск zero-copy-capable VP9/NV12 asset проходит.
- Ручной запуск искусственно unsupported zero-copy case падает typed ошибкой.

### Self-review

- Проверить, что ни один env var не может отключить zero-copy в production.
- Проверить, что `queue.write_texture` не используется для decoded video frames в
  production path.
- Проверить, что `FrameMemoryPath::CpuUpload` не проходит через renderer.
- Проверить, что capability UI/snapshot не обещает CPU fallback как поддержку.
- Проверить, что ошибки не игнорируются и не превращаются в silent drop.
- Проверить, что изменения не привязаны к VP9 и применимы к будущим codec-ам.

### Остановиться и спросить

- Если для tests нужен CPU upload helper, но непонятно, допустим ли отдельный
  compile-time feature.
- Если какой-то поддерживаемый сейчас asset перестает запускаться из-за
  отсутствия zero-copy support.

## Сессия 2. Playback Diagnostics and Drop Attribution

### Контекст для копипаста

Реализуй Session 2 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: добавить диагностику, которая объясняет каждый late drop и каждую паузу
pipeline. Нельзя оптимизировать вслепую. Метрики должны показывать demux, decode,
sync, import, queue depth, render acquire и drop reason. В конце сделай self-review
из этой сессии.

### Цель

Сделать дропы наблюдаемыми. После сессии должно быть ясно, где именно теряется
кадр: source/demux, decoder submit, decoder sync, zero-copy import, worker
scheduler, render acquire, GPU submit/present или release/backpressure.

### План реализации

- Ввести typed diagnostics model в `player-core`/`video-core` без зависимости от UI.
- Собирать per-frame или sampled timings:
  - demux read time;
  - decoder send queue depth;
  - decoder ready queue depth;
  - hardware sync latency;
  - DMA-BUF export/import latency;
  - present queue depth;
  - render acquire wait time;
  - GPU submit/present timing where available;
  - texture/surface slot pressure;
  - release acknowledgement latency.
- Собирать drop attribution:
  - dropped because late;
  - dropped because queue overflow;
  - dropped because stale generation;
  - dropped because seek/pre-roll;
  - dropped because render acquisition timeout;
  - dropped because decoder starvation.
- Добавить rolling counters и recent worst samples в `PlayerSnapshot`.
- Добавить debug log summary, который можно включить без massive log spam.
- Добавить UI/debug overlay только если он не смешивает business logic в
  `app-egui`; UI только читает snapshot.
- Добавить tests для aggregation/drop reason mapping.

### Acceptance

- Для каждого video drop есть typed reason.
- Snapshot показывает zero-copy memory path и основные queue depths.
- Есть worst latency counters по стадиям pipeline.
- Диагностика не требует CPU readback и не меняет frame ownership.
- `cargo check` и релевантные tests проходят.
- На 4k60 asset можно получить короткий diagnostics summary после воспроизведения.

### Self-review

- Проверить, что diagnostics не создает unbounded allocations на каждый кадр.
- Проверить, что timestamps используют monotonic clock.
- Проверить, что UI не вычисляет состояние pipeline самостоятельно.
- Проверить, что metrics names codec-neutral.
- Проверить, что drop reason не теряется при переходе через worker/event boundary.

### Остановиться и спросить

- Если потребуется выбрать формат user-facing diagnostics: log-only, overlay или
  отдельная debug panel.
- Если метрики требуют публичного API, который может стать стабильным контрактом.

## Сессия 3. Stable Zero-Copy Surface Pool

### Контекст для копипаста

Реализуй Session 3 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: убрать per-frame DMA-BUF import/destruction churn и заменить его
управляемым zero-copy surface/import pool. CPU fallback запрещен. Опирайся на
diagnostics из Session 2 и сохрани codec-neutral design. В конце сделай self-review
из этой сессии.

### Цель

Стабилизировать hot path: кадр не должен каждый раз создавать и уничтожать
дорогие external GPU resources без pool/lifecycle. Zero-copy должен быть не только
правильным, но и предсказуемым по latency.

### План реализации

- Описать единый lifecycle decoded surface:
  - decoded surface acquired;
  - DMA-BUF exported;
  - external image imported/wrapped;
  - frame leased to renderer;
  - GPU submitted;
  - GPU completion acknowledged;
  - surface/import returned to pool.
- Выделить отдельный module для zero-copy surface pool.
- Убрать ownership ambiguity между decoder texture cache, renderer lease и worker
  release ack.
- Переиспользовать imports, где backend guarantees позволяют это делать.
- Если persistent import требует explicit layout/ownership transitions, оформить
  это как явный backend contract, а не env experiment.
- Разделить два уровня результата:
  - обязательный уровень Session 3: bounded lifecycle pool, generation-safe lease
    и replace/import только после GPU completion;
  - optional backend-specific уровень: persistent `Reuse` без нового import-а
    только при доказанном explicit external-memory synchronization contract.
- Сделать pool bounded и observable:
  - active surfaces;
  - free surfaces;
  - waiting for GPU completion;
  - waiting for decoder reuse;
  - import failures;
  - imports created;
  - imports reused;
  - imports replaced.
- Добавить typed errors для нарушений lifecycle.
- Добавить tests на lease/release ordering и generation safety.

### Уточнение после Session 3

Текущий VA-API/Vulkan/wgpu path считается корректно завершенным для Session 3 как
safe surface/import lifecycle pool, но не как полный persistent import reuse.
Для этого backend-а `explicit_external_memory_reuse_sync = false`, потому что
пока нет отдельного дизайна явных VA writer -> Vulkan sampler ownership/layout/cache
transitions.

Нормальный текущий hot path для VA-API/wgpu:

- surface/import slot bounded и observable;
- decoded surface возвращается decoder-у только после renderer release и GPU
  completion;
- same surface может идти через `Replace`, даже если `surface_id` совпал;
- `imports_reused = 0` не является ошибкой для текущего backend-а;
- `imports_replaced > 0` является ожидаемой диагностикой безопасного replace path;
- CPU fallback при import failure остается запрещенным.

Persistent `Reuse` можно включать только для backend-а, который явно выставляет
contract уровня `explicit_external_memory_reuse_sync = true` и покрыт тестами или
manual verification на отсутствие stale/cyclic frames. Следующие сессии не должны
форсировать reuse ради latency, если этот contract не доказан.

### Acceptance

- Hot path не создает новый external import на каждый frame без необходимости,
  где "без необходимости" определяется backend reuse contract-ом. Для backend-а
  без explicit external-memory sync безопасный `Replace` после GPU completion
  считается корректным результатом Session 3.
- Surface/import reuse не ломает generation safety.
- Release после GPU completion возвращает ресурс в правильный pool.
- Texture/resource cleanup больше не зависит от forced per-frame cleanup как
  основного механизма выживания.
- Diagnostics показывают bounded lifecycle, `created/reused/replaced/failures` и
  стабильную ownership картину. Снижение import churn ожидается только там, где
  backend contract разрешает persistent `Reuse`.
- `cargo check` и релевантные tests проходят.

### Self-review

- Проверить, что pool не держит decoded surface дольше нужного и не душит decoder.
- Проверить, что stale frames после seek не возвращаются в неправильную generation.
- Проверить, что renderer не владеет decoder-specific objects напрямую.
- Проверить, что pool API не содержит VP9-specific naming.
- Проверить, что fallback при import failure остается fail-fast, а не CPU upload.

### Остановиться и спросить

- Если выбранный Vulkan/wgpu external memory path не позволяет безопасный reuse без
  отдельного backend-specific synchronization design.
- Если нужно менять public frame/renderer contract.

## Сессия 4. Decoder Thread Latency and Backpressure

### Контекст для копипаста

Реализуй Session 4 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: убрать decode-thread узкие места и неправильный backpressure, которые
мешают 4k60 burst-ам. CPU fallback запрещен. Не исправляй симптомы увеличением
drop thresholds без понимания причины. В конце сделай self-review из этой сессии.

### Цель

Сделать decode pipeline устойчивым к сложным сценам, крупным packets и burst-ам
готовых кадров. Decoder thread не должен простаивать из-за слишком малых очередей,
а worker не должен видеть starvation из-за искусственных лимитов.

### План реализации

- Использовать diagnostics Session 2 для определения реального bottleneck.
- Использовать Session 3 counters `imports_created`, `imports_reused`,
  `imports_replaced`, `waiting_gpu_completion`, `waiting_decoder_reuse` как
  входные данные backpressure. Не считать `imports_reused = 0` багом для
  VA-API/wgpu, пока `explicit_external_memory_reuse_sync = false`.
- Разделить стадии decoder thread:
  - packet receive;
  - hardware submit;
  - event drain;
  - hardware sync/fence wait;
  - zero-copy export/import;
  - decoded frame publish.
- Проверить `handle.sync()`:
  - измерить latency;
  - по возможности заменить blocking wait на fence/event-driven readiness;
  - если blocking wait неизбежен, вынести его так, чтобы не блокировать packet
    intake дольше необходимого.
- Пересмотреть queue sizes:
  - ready queue;
  - decoded frame channel;
  - frame/surface pool;
  - decoder packet channel.
- Убрать unbounded queue там, где она скрывает backpressure и memory growth.
- Сделать backpressure reason typed:
  - waiting for free surface;
  - waiting for present queue;
  - waiting for GPU release;
  - waiting for demux/audio priority.
- Не пытаться лечить import latency принудительным persistent reuse. Если
  replacements дают слишком большой latency, зафиксировать это как отдельную
  backend-specific synchronization/design задачу, а не как scheduler/backpressure
  workaround.
- Добавить tests на bounded behavior и shutdown/flush без deadlock.

### Acceptance

- Decoder thread не теряет throughput после одного latency spike.
- Worker видит backpressure reason, а не просто отсутствие frames.
- Queue limits documented и configurable where appropriate.
- Flush/shutdown остаются bounded по времени.
- 4k60 asset не показывает decoder starvation при наличии свободных ресурсов.
- `cargo check` и релевантные tests проходят.

### Self-review

- Проверить, что новые очереди не unbounded по умолчанию.
- Проверить, что ошибки decoder/import не проглатываются.
- Проверить, что timeout-ы имеют config/default/validation или локально
  документированы как backend constants.
- Проверить, что flush не оставляет leased surfaces.
- Проверить, что будущий AV1/HEVC backend сможет использовать те же контракты.

### Остановиться и спросить

- Если нужно менять threading model decoder backend-а.
- Если backend API требует выбор между lower latency и higher buffering.

## Сессия 5. Playback Scheduler Catch-Up and Present Policy

### Контекст для копипаста

Реализуй Session 5 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: перестроить scheduler под стабильный 4k60+ playback с запасом ресурсов.
Нельзя просто отключить drops или увеличить magic constants. Бюджеты должны быть
объяснимы, конфигурируемы и связаны с diagnostics. В конце сделай self-review из
этой сессии.

### Цель

Worker/tick scheduler должен уметь догонять pipeline после коротких latency spikes,
а не жить в режиме `2 packets + 2 decoded frames per 16.67ms` без компенсации.

### План реализации

- Перенести hardcoded tick budgets в config:
  - demux packets per tick;
  - video packets sent per tick;
  - decoded frames drained per tick;
  - present queue target/min/max;
  - decode-ahead target/max;
  - texture/surface free-slot watermarks.
- Добавить adaptive catch-up loop:
  - если worker опоздал или очереди ниже target, разрешить extra work до bounded
    time budget;
  - если render/present queue заполнена, не over-decode без пользы;
  - если decoder starvation, приоритет demux/decode выше UI diagnostics.
- Пересмотреть late-drop policy:
  - различать real late, stale generation и scheduler starvation;
  - не drop-ать frame, если root cause - временная нехватка ready frames;
  - не держать frame, который гарантированно ухудшит A/V sync.
- Сохранить audio clock как источник playback time, но явно документировать
  lead/grace windows.
- Добавить tests для scheduler:
  - one tick delayed;
  - decoder burst after delay;
  - present queue near empty;
  - present queue full;
  - seek generation transition.

### Acceptance

- Tick budgets больше не являются скрытыми magic constants.
- Scheduler может обработать burst decoded frames после latency spike.
- Drop counters показывают меньше late drops на 4k60 asset без ухудшения A/V sync.
- Config validation защищает от бессмысленных значений.
- `cargo check` и scheduler tests проходят.

### Self-review

- Проверить, что scheduler не превращен в одну большую функцию.
- Проверить, что config names не codec-specific.
- Проверить, что adaptive mode bounded и не может занять worker навсегда.
- Проверить, что seek/scrub behavior не сломан.
- Проверить, что drop policy документирована и покрыта tests.

### Остановиться и спросить

- Если нужно выбрать default latency profile: low-latency или smooth-playback.
- Если изменение scheduler tradeoff влияет на seek responsiveness.

## Сессия 6. Non-Blocking Render Acquisition

### Контекст для копипаста

Реализуй Session 6 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: убрать блокирующее ожидание worker из render hot path. Render должен
использовать последний безопасно опубликованный zero-copy frame/lease и не терять
кадр из-за 2ms request/reply timeout. CPU fallback запрещен. В конце сделай
self-review из этой сессии.

### Цель

Render loop должен быть предсказуемым. Он не должен каждый кадр зависеть от того,
успеет ли worker ответить на synchronous request.

### План реализации

- Проанализировать текущий render-frame request/reply boundary.
- Ввести latest-present-frame handoff:
  - worker публикует последний готовый frame lease descriptor;
  - render thread берет его non-blocking;
  - previous frame может быть reused, если новый frame еще не опубликован;
  - release остается RAII/ack based и generation-safe.
- Убрать hot-path timeout как нормальный механизм кадра.
- Сделать explicit state для:
  - no frame yet;
  - reused previous frame;
  - new frame acquired;
  - stale frame rejected;
  - render error reported.
- Обновить diagnostics:
  - render acquisition latency;
  - reused frame count;
  - worker response timeout count should go to zero or become legacy-only.
- Добавить tests для lease lifecycle и stale generation.

### Acceptance

- Render frame acquisition не блокирует hot path на worker reply.
- Reuse previous frame не нарушает release/ownership.
- Zero-copy frame handles остаются valid до GPU completion.
- Diagnostics подтверждают отсутствие render acquire timeout drops.
- `cargo check` и релевантные tests проходят.

### Self-review

- Проверить, что render thread не получил business logic player-а.
- Проверить, что lifetime frame-а не стал unsafe/неявным.
- Проверить, что seek/stale generation не может показать старый frame как новый.
- Проверить, что release ack не теряется при reuse.
- Проверить, что изменения не завязаны на VP9/NV12.

### Остановиться и спросить

- Если non-blocking handoff требует менять public `PlayerWorker` API.
- Если нужно выбрать между frame reuse и rendering blank on starvation.

## Сессия 7. Codec-Neutral Video Pipeline Contracts

### Контекст для копипаста

Реализуй Session 7 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: закрепить codec-neutral контракты, чтобы будущие AV1/HEVC/другие codec-и
не копировали VP9-специфику в `player-core` или renderer. Zero-copy остается
обязательным. В конце сделай self-review из этой сессии.

### Цель

Подготовить архитектуру к будущим codec-ам без повторения текущих проблем:
codec-specific probing и requirements должны жить в adapters, а общие слои должны
работать с typed decode requirements, frame contracts и capabilities.

### План реализации

- Проверить, где VP9-specific logic просочилась в общие слои.
- Оформить общие типы:
  - `VideoDecodeRequirement`;
  - `VideoSurfaceFormat`;
  - `VideoMemoryContract`;
  - `ZeroCopyExportRequirement`;
  - `ColorPipelineRequirement`;
  - `FrameTimingContract`.
- Сделать codec adapters ответственными за:
  - profile/level/bit-depth/chroma validation;
  - codec private/header parsing;
  - hardware backend requirement;
  - color metadata extraction/confidence.
- Сделать capability intersection codec-neutral:
  - decoder backend capabilities;
  - renderer import/render capabilities;
  - color pipeline capabilities;
  - platform restrictions.
- Обновить docs/project map под будущие codec-и.
- Добавить tests на rejection unsupported format до decode start.

### Acceptance

- `player-core` не содержит VP9-specific parsing или profile branching.
- Новый codec можно добавить через adapter без переписывания scheduler/render lease.
- Zero-copy requirement выражен как общий contract.
- Unsupported codec/profile получает typed reject до запуска heavy pipeline.
- `cargo check` и relevant tests проходят.

### Self-review

- Проверить dependency direction между core crates и codec adapters.
- Проверить, что имена типов не завязаны на VP9/NV12/P010 без необходимости.
- Проверить, что color metadata path сохраняет confidence/origin.
- Проверить, что будущий codec не сможет включить CPU fallback обходным путем.
- Проверить, что docs объясняют extension path.

### Остановиться и спросить

- Если нужно выбрать naming/public API, который станет долгоживущим.
- Если текущий VP9 path придется временно обернуть adapter-ом с заметным diff.

## Сессия 7.5. Media-Clock-Driven Worker Wakeup

  ### Контекст для копипаста

  Реализуй новую Session 7.5 для smooth playback: убрать жёсткую привязку playback worker wakeup к фиксированному 60Hz tick. Сейчас worker использует `DEFAULT_WORKER_TICK_INTERVAL = 16_667us`, но media cadence должна идти от frame PTS/
  audio clock/decode readiness, а не от предположения “видео всегда 60 fps”.

  Перед правками обязательно:
  - задать project path через MCP `code_index`;
  - сделать deep index;
  - свериться с Context7 по `winit`/event-loop timing и релевантным Rust API, если меняешь timing/wakeup модель;
  - сначала предложить архитектуру, потом реализовать;
  - искать причину drops, а не маскировать счётчики.

  Исходный кейс:
  - asset: `test-assets/4k60fps_sdr/LXb3EKWsInQ_2160p60_sdr_vp9_opus.webm`;
  - `ffprobe` показывает фактический video rate около `59.939827 fps`;
  - packet PTS cadence за первые 120s чистая: интервалы в основном `16ms/17ms`, bad intervals `0`;
  - значит файл не выглядит битым, а 59.94 на 60Hz display должен давать редкие repeated refreshes, но не late media drops.

  ### Цель

  Сделать worker scheduling media-clock-driven:

  - worker не должен постоянно тикать с fixed 60Hz как с источником истины;
  - следующий wakeup должен вычисляться из состояния pipeline:
    - audio/media clock;
    - PTS первого queued video frame;
    - допустимое present lead/window;
    - decode/demux backpressure;
    - seek/preroll/opening state;
    - render release/acquire events;
  - 59.94 fps, 60 fps, 24 fps, 30 fps и future VFR должны проходить через один PTS-based scheduler;
  - repeated frames должны учитываться отдельно от dropped media frames.

  ### План реализации

  - Найти все места, где playback timing завязан на fixed 60Hz:
    - `DEFAULT_WORKER_TICK_INTERVAL`;
    - `position_fallback_delta`;
    - worker `next_tick_timeout`;
    - tests, которые предполагают fixed tick.
  - Спроектировать функцию вычисления следующего worker wakeup:
    - если pipeline idle, worker спит до command/release/acquire event;
    - если есть ready frame, следующий wakeup считается от `front_frame.pts` относительно presentation clock;
    - если очередь ниже target или есть decoder/demux work, wakeup должен быть immediate/near-immediate для bounded catch-up;
    - если frame ещё рано показывать, worker ждёт до ближайшего meaningful deadline;
    - если audio отсутствует, использовать monotonic media clock fallback, но не hardcoded 60Hz как video cadence.
  - Не менять zero-copy contract и не добавлять CPU fallback.
  - Не “лечить” smoothness отключением late drops или увеличением magic thresholds.
  - Обновить diagnostics так, чтобы было видно:
    - worker wakeup reason;
    - planned wakeup delay;
    - tick lateness;
    - media clock vs front frame PTS diff;
    - repeated frame count отдельно от drops.
  - Добавить unit tests:
    - 59.94 cadence не накапливает искусственные late drops;
    - 60.0 cadence не регрессирует;
    - 24/30 fps не заставляют worker бессмысленно тикать 60 раз/сек;
    - late drop происходит только когда есть replacement frame и frame реально вышел за grace;
    - starvation/repeat не записывается как late drop.
  - Обновить docs/manual protocol:
    - repeated frames допустимы на refresh/video mismatch;
    - late drops в steady-state должны стремиться к нулю;
    - thresholds должны быть объяснены через frame duration/PTS, а не через “60 fps”.

  ### Acceptance

  - Worker wakeup больше не использует fixed 60Hz как единственный playback cadence source.
  - Scheduler остаётся PTS/audio-clock based.
  - 59.94 fps asset не получает late drops только из-за разницы с 60Hz display/worker cadence.
  - Repeated frames считаются отдельно и не смешиваются с dropped media frames.
  - Diagnostics показывают причину wakeup и diff между target media time и front frame PTS.
  - `cargo check` проходит.
  - Релевантные unit tests проходят.
  - Manual run 4k60 SDR VP9/NV12 показывает:
    - zero-copy path;
    - no CPU fallback;
    - no steady-state `Late` drops без объяснённой причины;
    - repeats допускаются и documented.

  ### Реализация

  #### Зафиксированный результат на 2026-05-15

  - Линейное воспроизведение без seek/scrub выведено в стабильное состояние:
    полный manual-прогон 4k60 SDR VP9/Opus от начала до конца проходит без
    media drops.
  - Основная причина прежних steady-state late drops была не в битом asset и не
    в 59.94fps cadence, а в том, что worker использовал fixed 60Hz tick как
    фактический источник playback cadence.
  - После перехода на PTS/audio-clock scheduler 59.94fps, 60fps, 30fps, 24fps и
    будущий VFR проходят через одну модель: deadline считается от media time и
    `front_frame.pts`, а не от предположения, что следующий кадр обязан прийти
    через `16_667us`.
  - Дополнительная найденная причина пачечных проблем после первого варианта
    фикса: `PipelineWorkReady` был слишком широким и мог будить worker с `0ms`,
    даже когда present queue уже была здоровой/полной, а первый queued frame ещё
    находился в future относительно media clock. Это создавало tight loop,
    нагружало worker и могло проявляться пачкой проблем после нескольких секунд
    нормального playback.
  - Этот busy-spin устранён: pending decode/demux work больше не превращается в
    immediate wakeup, если present queue уже достигла target или в ней нет
    свободных present slots. В таком состоянии worker ждёт ближайший
    PTS-deadline либо внешний command/render event.
  - `Condvar::wait_timeout`/worker timeout трактуется только как best-effort
    ожидание до ближайшего meaningful deadline. После любого wakeup состояние
    pipeline пересчитывается заново, потому что timeout не является точным
    frame clock и может проснуться из-за event, scheduler delay, platform
    latency или spurious wakeup.
  - Repeated/reused frames считаются отдельно от dropped media frames. Refresh
    mismatch, например 59.94fps video на 60Hz display, может давать редкие
    repeats, но не должен сам по себе превращаться в steady-state `Late` drops.

  - Worker больше не использует `16_667us` как cadence playback. `PlayerWorker`
    получает read-only план от `PlayerSession::worker_wakeup_plan(...)` и ждёт:
    - command/render/scrub/shutdown event без timeout, когда pipeline idle;
    - `front_frame.pts - present_lead` относительно текущего media clock, когда
      следующий кадр ещё рано публиковать;
    - immediate wakeup для bounded demux/decode work, когда очереди ниже target;
    - healthy presentation queue не превращает pending decode/demux work в
      `0ms` wakeup: пока первый queued frame ещё future, worker ждёт media
      deadline и не busy-spin-ит на полной очереди;
    - короткий `decoder_readiness_poll_interval`, пока decoder thread отдаёт
      frames через неблокирующий `try_recv_frame()`;
    - редкий `coarse_wakeup_interval` только как progress fallback без media
      cadence semantics.
  - No-audio playback использует внутренний monotonic media clock anchor:
    position считается от `Instant`, а не через прибавление fixed 60Hz delta на
    каждом worker wakeup.
  - `Late` drop по-прежнему возможен только когда первый queued frame реально
    вышел за grace и есть replacement frame. Starvation и повтор текущего frame
    считаются separately и не увеличивают late-drop counters.
  - Diagnostics snapshot теперь показывает:
    - `worker_wakeup.reason`;
    - `worker_wakeup.planned_delay`;
    - `worker_wakeup.tick_late_by`;
    - `worker_wakeup.frame_timing.front_frame_delta_from_target_us`;
    - `repeated_video_frames` отдельно от `drops`.

  ### Known issue после 7.5: seek/scrub

  - Scope 7.5 считается закрытым для обычного линейного playback: steady-state
    playback без seek не даёт пропусков на исходном 4k60 SDR VP9/Opus asset.
  - После ручного протягивания seek/scrub может стать нестабильным: playback
    иногда залипает после drag, а повторное протягивание может вывести pipeline
    из этого состояния.
  - Этот seek issue не нужно чинить откатом PTS scheduler-а и не нужно
    маскировать через отключение late drops. Следующая сессия должна отдельно
    проверить seek generation, flush/preroll, render lease release/acquire,
    decoder readiness и wakeup reason после scrub.
  - Для будущего расследования важно различать:
    - steady-state media cadence, который теперь PTS/audio-clock based и
      подтверждён manual-прогоном;
    - transition-state после seek/scrub, где возможна отдельная проблема с
      generation handoff, stale frames, drained queues или отсутствующим wakeup
      после preroll.

  ### Manual protocol

  - Запустить SDR VP9/NV12 asset:
    `cargo run -p app-egui -- test-assets/4k60fps_sdr/LXb3EKWsInQ_2160p60_sdr_vp9_opus.webm`.
  - Для проверки именно Session 7.5 не трогать seek/scrub: этот protocol
    валидирует steady-state PTS/audio-clock playback, а не transition-state
    после ручного протягивания.
  - В telemetry/diagnostics проверить:
    - `Memory path` остаётся zero-copy (`DmaBufZeroCopy`);
    - CPU fallback не появляется;
    - `Wake` в steady-state в основном идёт через `frame_pts_deadline`, а не
      fixed-rate polling;
    - `Wake delay` следует PTS cadence между соседними lead-deadline-ами:
      для 59.94 это ожидаемые интервалы порядка `16ms/17ms`, для 30fps около
      `33ms`, для 24fps около `41ms`. Первый wakeup перед первым queued frame
      может быть короче, потому что scheduler просыпается на `PTS - present_lead`,
      а не ровно в PTS;
    - `PTS-target` остаётся малым и объяснимым через PTS/frame duration;
    - `Repeated/reused` и `Worker repeats` могут расти при refresh/video mismatch;
    - steady-state `Late` drops должны стремиться к нулю. Если `Late` растёт,
      смотреть `Wake late`, `PTS-target`, queue depths и decoder/render latency,
      а не списывать это на 59.94 vs 60Hz mismatch.

  ### Self-review

  - Проверить, что код не подогнан только под VP9 или только под 59.94.
  - Проверить, что нет нового hardcoded `16_667us` как media cadence.
  - Проверить, что fixed interval может остаться только как coarse idle fallback, если это явно документировано.
  - Проверить, что drops не скрываются и не переименовываются в repeats.
  - Проверить, что render loop и worker loop не начали busy-spin.
  - Проверить, что tests не требуют реальный GPU или большой media asset.

  ### Остановиться и спросить

  - Если нужно выбрать между двумя архитектурами:
    - worker сам планирует exact media deadlines;
    - render loop/worker делят ответственность за frame deadline.
  - Если для no-audio media нужен новый public config/contract для monotonic media clock fallback.
  - Если smoothness threshold требует продуктового решения: сколько repeated refreshes допустимо при 59.94 видео на 60Hz display.

## Сессия 8. Smooth Playback Regression Suite

### Контекст для копипаста

Реализуй Session 8 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: добавить regression/stress проверки, которые защищают smooth playback и
zero-copy invariant. Тесты не должны требовать коммита больших media files в repo;
большие assets остаются external/manual с documented expected metrics. В конце
сделай self-review из этой сессии.

### Цель

Не потерять достигнутую плавность после будущих изменений. Проверки должны ловить
возврат CPU fallback, рост late drops, неправильный release lifecycle и regression
в scheduler.

### План реализации

- Добавить unit tests для:
  - zero-copy contract validation;
  - capability intersection;
  - scheduler catch-up;
  - drop reason attribution;
  - lease/release lifecycle.
- Добавить integration-style tests без больших assets:
  - fake decoder backend with configurable latency spikes;
  - fake renderer release timing;
  - fake demux burst source.
- Добавить manual stress protocol:
  - 4k60 SDR VP9/NV12;
  - 4k60 HDR VP9.2/P010;
  - future AV1/HEVC entries as placeholders;
  - local file and YouTube/network variants;
  - expected diagnostics thresholds.
- Добавить zero-copy guard:
  - test/log assertion that no decoded video frame uses CPU upload;
  - no production env var can disable zero-copy;
  - renderer rejects non-zero-copy video frames.
- Документировать команды ручной проверки и формат результата.

### Manual stress protocol

Ручной protocol валидирует только production path. Он не включает software video
decode, CPU upload/readback, hidden env compatibility shims или отключение drop
accounting.

Базовый лог:

```bash
RUST_LOG=info,player_core::worker=debug cargo run -p app-egui -- <asset-or-url>
```

Локальные supported assets:

```bash
cargo run -p app-egui -- test-assets/4k60fps_sdr/LXb3EKWsInQ_2160p60_sdr_vp9_opus.webm
cargo run -p app-egui -- test-assets/hdr/LXb3EKWsInQ_2160p60_hdr_vp9_profile2.webm
cargo run -p app-egui -- test-assets/hdr/rs-U-zKZyks_2160p60_hlg_vp9_profile2_opus.webm
```

YouTube/network variant использует тот же diagnostics contract, но source
starvation считается отдельной внешней причиной. Network run нельзя использовать
для изменения decoder/render defaults, пока local file run не показал тот же
bottleneck.

Ожидаемые checks:

- `memory_path = Some(DmaBufZeroCopy)`;
- `import_failures = Some(0)`;
- отсутствуют CPU upload/readback diagnostics;
- steady-state `drops_late = 0`;
- `drops_decoder_starvation = 0` для local file;
- `wake_reason` в playback обычно `frame_pts_deadline`, а не fixed polling;
- `render_acquire_worst_ms` и `gpu_submit_present_worst_ms` сравниваются с
  frame budget и queue depths, но сами по себе не являются drop threshold;
- `repeated_video_frames` анализируется отдельно от media drops.

### Acceptance

- Regression suite ловит CPU fallback.
- Fake latency tests воспроизводят scene-change-like spikes без реального media.
- Manual 4k60 protocol documented and repeatable.
- Diagnostics thresholds понятны и не являются магическими.
- `cargo check` и tests проходят.

### Self-review

- Проверить, что tests не flaky и не завязаны на конкретный GPU без маркировки.
- Проверить, что большие assets не попали в repo.
- Проверить, что fake backend не стал вторым production pipeline.
- Проверить, что manual protocol содержит expected logs/metrics.
- Проверить, что future codec placeholders не обещают поддержку до реализации.

### Остановиться и спросить

- Если нужно выбрать набор external media samples.
- Если smoothness thresholds требуют проектного решения по допустимому числу
  repeated frames/drops.

## Сессия 9. Final 4k60+ Tuning and Architecture Review

### Контекст для копипаста

Реализуй Session 9 из `docs/rustiplayer/12-smooth-playback-zero-copy-sessions.md`.
Задача: финальная настройка smooth playback после zero-copy lockdown, diagnostics,
surface pool, decoder/backpressure, scheduler и render acquisition. Не добавляй
новых архитектурных обходов. Используй metrics, manual stress protocol и
self-review из этой сессии.

### Цель

Закрыть остаточные причины micro-stutter на поддерживаемых 4k60+ видео и привести
документацию к фактической архитектуре.

### План реализации

- Прогнать manual stress protocol из Session 8.
- Сравнить diagnostics по стадиям:
  - source/demux;
  - decoder sync;
  - import/pool;
  - worker scheduler;
  - render acquire;
  - GPU submit/present;
  - release/backpressure.
- Настроить defaults только на основании measurements.
- Удалить временные compatibility shims, если они больше не нужны.
- Обновить architecture docs:
  - zero-copy invariant;
  - supported formats;
  - known unsupported formats;
  - tuning defaults;
  - diagnostics usage.
- Провести self-review по всем предыдущим session invariants.

### Acceptance

- Поддерживаемые 4k60 assets воспроизводятся без visible late drops в manual test.
- Diagnostics не показывает CPU upload/readback.
- Late drops либо равны нулю на steady-state, либо имеют объясненную внешнюю
  причину, например source starvation.
- Defaults documented и проходят validation.
- Architecture docs соответствуют коду.
- `cargo check`, relevant tests и manual protocol завершены.

### Self-review

- Проверить, что smoothness достигнута не отключением drop accounting.
- Проверить, что zero-copy invariant не ослаблен ради совместимости.
- Проверить, что future codec path не заблокирован VP9-specific решениями.
- Проверить, что диагностика достаточна для следующего performance incident.
- Проверить, что документация не обещает неподдерживаемые форматы.

### Остановиться и спросить

- Если remaining stutter находится ниже уровня приложения, например driver,
  compositor, kernel, display mode или hardware decoder firmware.
- Если требуется принять продуктовый threshold для "идеально плавно".

### Реализация

#### Зафиксированный результат на 2026-05-15

- Добавлен последний недостающий stage metric: `render-wgpu` измеряет время от
  `queue.submit()` до возврата из `surface_texture.present()`, `app-egui`
  передает sample в `PlayerWorker`, а `player-core` записывает его как
  `PipelineLatencyStage::GpuSubmitPresent`.
- Diagnostics summary теперь выводит typed drop counters, typed pause counters,
  repeated frames, worker wakeup reason/delay/lateness, per-stage worst latency и
  zero-copy surface pool counters.
- Runtime compatibility shim `RUSTIPLAYER_DEV_VERIFY_P010_BOUNDARY` удалён.
  Production HDR/P010 больше нельзя включить обходом renderer/HDR capability
  gate: stream проходит только через обычный `check_video_requirement()`.
- Defaults не менялись. Measurements не показали steady-state bottleneck,
  который требовал бы увеличения queue/pool/scheduler knobs.

#### Manual stress measurements

Команды запускались с bounded `timeout`, поэтому exit code `124` у успешного
manual run означает только остановку теста по времени после steady-state logs.

SDR VP9 Profile 0, NV12, 4k60:

```bash
timeout 35s env RUST_LOG=info,player_core::worker=debug \
  cargo run -p app-egui -- \
  test-assets/4k60fps_sdr/LXb3EKWsInQ_2160p60_sdr_vp9_opus.webm
```

Итог steady-state diagnostics:

- `drops = 0`, `drops_late = 0`;
- `memory_path = Some(DmaBufZeroCopy)`;
- `import_failures = Some(0)`;
- `demux_worst_ms = Some(1.799079)`;
- `decoder_submit_worst_ms = Some(15.766128)`;
- `decoder_sync_worst_ms = Some(0.044853)`;
- `import_worst_ms = Some(1.205236)`;
- `worker_worst_ms = Some(0.114832)`;
- `render_acquire_worst_ms = Some(0.031686)`;
- `gpu_submit_present_worst_ms = Some(4.927615)`;
- `release_ack_worst_ms = Some(1.576332)`;
- `present_queue_depth = 6`;
- `texture_capacity = Some(24)`, `texture_free = Some(13)`,
  `texture_waiting_gpu = Some(0)`;
- `imports_created = Some(1227)`, `imports_replaced = Some(1206)`,
  `imports_reused = Some(0)`, `import_failures = Some(0)`;
- `repeated_video_frames = 12`, отдельно от media drops.

HDR VP9 Profile 2, P010, 4k60:

```bash
timeout 20s env RUST_LOG=info,player_core::worker=debug \
  cargo run -p app-egui -- \
  test-assets/hdr/LXb3EKWsInQ_2160p60_hdr_vp9_profile2.webm
```

Итог steady-state diagnostics:

- stream прошёл production path без `RUSTIPLAYER_DEV_VERIFY_P010_BOUNDARY`;
- `drops = 0`, `drops_late = 0`;
- `memory_path = Some(DmaBufZeroCopy)`;
- `import_failures = Some(0)`;
- `demux_worst_ms = Some(3.570043)`;
- `decoder_submit_worst_ms = Some(22.754762)`;
- `decoder_sync_worst_ms = Some(0.020664)`;
- `import_worst_ms = Some(1.990385)`;
- `worker_worst_ms = Some(0.565628)`;
- `render_acquire_worst_ms = Some(0.10512)`;
- `gpu_submit_present_worst_ms = Some(9.26855)`;
- `release_ack_worst_ms = Some(1.988161)`;
- `imports_created = Some(1017)`, `imports_replaced = Some(996)`,
  `imports_reused = Some(0)`, `import_failures = Some(0)`.

У HDR sample без audio высокий `repeated_video_frames` ожидаем как accounting
повтора текущего кадра, а не как media drop. Для network/YouTube variants
`source/demux` starvation должен считаться отдельной причиной и не использоваться
для изменения local decoder/render defaults без совпадающего local bottleneck.

#### Session 9 self-review result

- Smoothness достигнута не отключением counters: `Late`, queue, stale,
  seek-preroll и decoder-starvation drops продолжают считаться раздельно.
- Zero-copy invariant не ослаблен: CPU upload/readback не добавлены, а P010
  diagnostic env shim удалён.
- Future codec path не заблокирован VP9-specific решением: новая метрика
  `GpuSubmitPresent` codec-neutral и подключена через render/worker boundary.
- Diagnostics теперь достаточно детализированы для следующего incident:
  видно source/demux, decoder, sync, import/pool, worker, render acquire,
  GPU submit/present и release/backpressure.
- Docs не обещают AV1/HEVC/H.264/VP8, native HDR output, wide-gamut SDR,
  VP9 12-bit или 4:2:2/4:4:4 как production support.
