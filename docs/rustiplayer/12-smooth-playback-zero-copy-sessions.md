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
