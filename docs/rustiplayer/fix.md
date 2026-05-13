
  # Задача: исправить архитектурные и perf-проблемы в `<REPO_ROOT>`

  Ты работаешь в проекте `<REPO_ROOT>`.

  ## Обязательные правила

  - Используй `MCP code_index` перед анализом.
  - Перед правками кода сверяйся с Context7, если затрагиваешь библиотеки/API/SDK.
  - Сначала найди причину проблемы, не лечи следствие.
  - Сначала предложи архитектуру изменения, затем реализуй.
  - Если нужно принять важное архитектурное решение, остановись и спроси.
  - Пиши production-ready код, в стиле существующего проекта.
  - Не делай unrelated refactor.
  - После каждой сессии:
    - запусти релевантные проверки;
    - сделай self-review;
    - исправь найденные недочёты;
    - кратко перечисли изменённые файлы и остаточные риски.

  ## Исходное ревью

  Нужно исправить проблемы, найденные в полном code review:

  1. Лишние копии encoded packet buffers:
     - `crates/webm-demux/src/symphonia_demuxer.rs:252`
     - `crates/player-core/src/tick.rs:487`
     - `crates/player-core/src/tick.rs:498`
     - `crates/video-vaapi/src/decoder_thread.rs:384`

  2. HTTP streaming path аллоцирует и копирует на каждый range read:
     - `crates/source-core/src/http.rs:230`
     - `crates/source-core/src/http.rs:406`
     - `crates/source-core/src/cache.rs:165`

  3. `PlayerSession` стал god object:
     - `crates/player-core/src/session.rs:61`
     - `crates/player-core/src/session.rs:67`
     - `crates/player-core/src/session.rs:248`
     - `crates/player-core/src/session.rs:970`

  4. YouTube startup может блокировать UI:
     - `crates/service-youtube/src/lib.rs:425`
     - `crates/app-egui/src/main.rs:591`

  5. `AppState::player_snapshot()` имеет side effect и вызывается несколько раз за frame:
     - `crates/app-egui/src/state.rs:160`
     - `crates/app-egui/src/state.rs:245`
     - `crates/app-egui/src/state.rs:323`

  6. `VideoDecodeThread::flush()` может зависнуть навсегда:
     - `crates/video-vaapi/src/decoder_thread.rs:319`

  7. `texture_cache` использует `Vec::remove` + ребейз индексов и сбрасывает handle counter:
     - `crates/video-vaapi/src/texture_cache.rs:1064`
     - `crates/video-vaapi/src/texture_cache.rs:1072`
     - `crates/video-vaapi/src/texture_cache.rs:1107`

  8. Demuxer глотает ошибки и предполагает VP9 для unknown video:
     - `crates/webm-demux/src/symphonia_demuxer.rs:358`
     - `crates/webm-demux/src/symphonia_demuxer.rs:457`

  9. Worker channel topology сложная:
     - `crates/player-core/src/worker.rs:258`
     - `crates/player-core/src/worker.rs:406`
     - `crates/player-core/src/worker.rs:575`

  10. App всегда в `ControlFlow::Poll` и постоянно делает redraw:
     - `crates/app-egui/src/main.rs:560`
     - `crates/app-egui/src/main.rs:331`

  ## Сессия 1: hot path buffer copies

  Цель: убрать лишние копии packet data в demux -> player-core -> decoder thread.

  Сначала предложи архитектуру ownership:
  - какой тип должен хранить encoded packet payload;
  - можно ли использовать `bytes::Bytes` сквозь `media-core`, `player-core`, `video-vaapi`;
  - где допустимы clone без copy;
  - какие API нужно поменять.

  Затем реализуй:
  - `PendingAudioPacket` / `PendingVideoPacket` должны не требовать `Vec<u8>`, если можно
  безопасно передавать `Bytes`;
  - `DecodePacket` должен не копировать payload обратно в `Bytes`;
  - сохранить поведение seek generation/keyframe/PTS;
  - добавить или обновить тесты на routing packet payload.

  Проверки:
  - `cargo check --workspace`
  - релевантные `cargo test -p player-core -p webm-demux -p video-vaapi`
  - если возможно, targeted clippy по затронутым crates.

  ## Сессия 2: HTTP range read/cache copies

  Цель: уменьшить аллокации и копии в `source-core`.

  Сначала предложи архитектуру:
  - как читать HTTP response напрямую в caller buffer;
  - как сохранить retry/cancellation/error semantics;
  - как cache должен хранить данные без лишней копии там, где это возможно;
  - не ломать `ByteSource` contract.

  Затем реализуй:
  - `HttpRangeSource::read()` не должен обязательно создавать промежуточный `Vec<u8>`;
  - `read_exact_response_body()` либо убрать, либо заменить direct-read helper;
  - cache path должен копировать только там, где реально нужно владение;
  - добавить тесты на partial read, retry, cancellation и cache hit/miss.

  Проверки:
  - `cargo test -p source-core`
  - `cargo check --workspace`

  ## Сессия 3: YouTube startup и service split

  Цель: убрать блокировку UI/startup и сделать `service-youtube` менее монолитным.

  Сначала предложи архитектуру:
  - где должен жить async/background resolve;
  - как UI показывает pending/error state;
  - как отменять или ограничивать `yt-dlp`;
  - какие модули выделить из `service-youtube/src/lib.rs`.

  Затем реализуй минимально безопасный вариант:
  - добавить timeout для `yt-dlp`;
  - не блокировать создание окна/UI при CLI URL;
  - вынести resolver/process/http-refresh/DTO в отдельные модули, если это не раздувает
  diff;
  - ошибки должны доходить до UI понятным образом.

  Проверки:
  - `cargo test -p service-youtube`
  - `cargo check --workspace`

  ## Сессия 4: PlayerSession boundaries

  Цель: уменьшить god object и отделить core state machine от IO/backend binding.

  Сначала предложи архитектуру и остановись, если есть несколько равнозначных вариантов.

  Ожидаемое направление:
  - скрыть `pub pipeline`, заменить явными методами;
  - вынести open local file / open demuxer orchestration из `PlayerSession` или хотя бы
  отделить source opening от state transition;
  - вынести VAAPI-specific init из core API за trait/factory boundary;
  - разделить session state, media opening, pipeline reset, seek state.

  Реализация должна быть поэтапной:
  - сначала инкапсулировать `pipeline`;
  - затем вынести source/backend binding;
  - затем уменьшить размер `session.rs`.

  Проверки:
  - `cargo test -p player-core`
  - `cargo check --workspace`

  ## Сессия 5: decoder robustness и texture cache

  Цель: убрать зависания и хрупкость texture handle/cache.

  Сначала предложи архитектуру:
  - timeout/cancel policy для `VideoDecodeThread::flush()`;
  - как безопасно сигнализировать fatal decoder thread state;
  - как заменить indexed `Vec` + handle rebasing;
  - надо ли сделать monotonically increasing handle без reset.

  Затем реализуй:
  - `flush()` не должен ждать бесконечно;
  - stale handles не должны получить новый frame после invalidate;
  - release imported slots не должен требовать O(n) rebasing, если можно избежать;
  - сохранить GPU completion lifetime guarantees.

  Проверки:
  - `cargo test -p video-vaapi`
  - `cargo check --workspace`

  ## Сессия 6: demuxer correctness

  Цель: сделать demuxer fail-safe.

  Сначала предложи политику:
  - сколько corrupted packets можно skip-ать подряд;
  - какие ошибки fatal;
  - как определять codec для unknown video;
  - что делать с unsupported codec.

  Затем реализуй:
  - bounded corrupted packet skipping;
  - не считать unknown video автоматически VP9 без доказательства;
  - добавить понятные ошибки;
  - покрыть тестами.

  Проверки:
  - `cargo test -p webm-demux`
  - `cargo check --workspace`

  ## Сессия 7: worker topology и render pacing

  Цель: убрать хрупкость worker channels и idle CPU/GPU burn.

  Сначала предложи архитектуру:
  - scrub coalescing без скрытого receiver drain в sender;
  - bounded release path или backpressure strategy;
  - render request timeout policy;
  - когда использовать `ControlFlow::Poll`, а когда `Wait`/conditional redraw.

  Затем реализуй:
  - сделать coalescing явно читаемым;
  - защитить release queue от бесконечного роста;
  - уменьшить redraw/poll в pause/idle без регрессии playback smoothness;
  - добавить тесты на worker command behavior, если есть подходящие unit seams.

  Проверки:
  - `cargo test -p player-core`
  - `cargo check --workspace`

  ## Сессия 8: lint cleanup и финальный self-review

  Цель: убрать подтверждённые clippy/design warnings, не трогая vendored код без
  необходимости.

  Проверить:
  - `clippy::result_large_err` в `crates/capability-core/src/selection.rs`;
  - `too_many_arguments` в render/Vulkan paths;
  - missing `# Safety` docs в unsafe Vulkan API;
  - warnings в project-owned crates.

  Не трать время на vendored `third_party` / `cros-*`, если это не мешает CI. Сначала
  спроси, считать ли их frozen upstream patch.

  Финальные проверки:
  - `cargo fmt --check`
  - `cargo check --workspace`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -W clippy::perf -W clippy::complexity -W
  clippy::suspicious`

  ## Итоговый результат

  В конце дай:
  - список исправленных проблем;
  - список изменённых файлов;
  - какие perf-регрессии устранены;
  - какие архитектурные границы стали чище;
  - какие риски остались;
  - какие проверки прошли.
