# 12. План усиления seek/scrub

Актуализировано: 2026-05-23.

Этот документ разбивает исправление seek/scrub на короткие сессии. Его можно
прикладывать к каждой новой сессии вместе с названием нужного шага.

Цель: сохранить уже хорошую плавность текущего механизма и убрать редкие
залипания, ложные дропы и слабые места расширяемости без переписывания рендера.

## Текущий статус, 2026-05-23

Старый live drag/preview core удалён из текущего runtime. Документ ниже остаётся
историческим планом и источником технического контекста, но active acceptance
теперь такая:

- ordinary click seek идёт через normal final seek;
- drag release временно коммитит latest pointer target через normal final seek;
- slow/fast live preview не является текущей фичей и не входит в PASS/FAIL
  критерии до будущей переписи с новой архитектурой.

## Общие правила для каждой сессии

- Перед правками задать рабочую директорию в `code_index`:
  `<REPO_ROOT>`.
- Перед правками построить deep index через `code_index`.
- Перед любыми правками кода свериться с Context7. Если сессия не затрагивает
  внешний API, явно зафиксировать это после проверки области правок.
  Для seek это в первую очередь Symphonia; для UI это может быть egui/winit.
- Искать причину, а не симптом. Не увеличивать буферы "на всякий случай".
- Не менять renderer, если сессия не доказывает, что проблема именно в boundary
  render lease/import.
- Не смешивать несколько архитектурных решений в одной сессии.
- После реализации запустить targeted tests и общий smoke:
  `cargo test -p player-core -p webm-demux -p app-egui`.
- В конце каждой сессии сделать self-review: что изменилось, какие инварианты
  защищены тестами, какие риски остались.

## Baseline из ревью

Seek уже хорошо отделён от UI и renderer:

- UI отправляет typed commands и читает `TimelineSnapshot`.
- Worker владеет `PlayerSession` и coalesces scrub updates latest-wins.
- Renderer получает только `PresentFrameLease`.
- Demux seek возвращает coarse/accurate позицию, а player владеет preroll/drop.

Основные найденные риски:

- seek продолжается после ошибки `video_decoder_thread.flush()`;
- во время seek decoder output может не дренироваться из-за present backpressure;
- публичный `SeekMode` для video фактически игнорируется;
- preview timeout может снять `stale_frame` без свежего кадра;
- generation в `SeekController` не защищает async preview/final intent;
- telemetry смешивает seek-discard и настоящие frame drops;
- EndScrub раньше имел неявную policy: часть веток тянулась к latest target,
  часть фактически коммитила последний видимый preview;
- codec/backend extensibility ограничена VP9/VA-API и Opus path.

## Заметка по миграции Symphonia 0.6, 2026-05-20

Demux path теперь использует upstream `symphonia = 0.6` без локального fork-а.
Конвертация packet-ов читает native `Packet.pts` и `Packet.dts`, поэтому DTS
больше не теряется на границе Symphonia -> `media-core::Packet`, когда контейнер
явно передаёт decode timestamp. Отрицательные raw timestamps по-прежнему
clamp-ятся для UI timeline через существующий mapper, чтобы пользовательская
позиция не уходила в negative duration.

## Сессия 1. Метрики и классификация seek-discard

### Задача

Развести настоящие playback/render drops и ожидаемые discard во время seek, чтобы
дальше отлаживать реальные потери, а не шум telemetry.

### Основные файлы

- `crates/player-core/src/tick.rs`
- `crates/player-core/src/session.rs`
- `crates/app-egui/src/main.rs`
- `crates/media-core/src/diagnostics.rs`, если причина drop уже проходит через
  общий diagnostics contract.

### Правки

- Проверить текущие причины `VideoFrameDropReason`.
- Отделить `SeekPreroll` и `StaleGeneration` от пользовательского счётчика
  dropped frames.
- Добавить отдельный счётчик или event для `seek_discarded_frames`.
- Не менять scheduler и seek behavior в этой сессии.

### Тесты

- Unit test на mapping telemetry в `app-egui`, где `SeekPreroll` не становится
  обычным playback drop.
- Existing player-core tests должны пройти без изменения смысла.

### Definition of Done

- После seek telemetry ясно показывает, что было discard, а что было реальным
  drop.
- Нет изменений в render path.

## Сессия 2. Fail-fast граница decoder flush

### Задача

Сделать ошибку flush явной границей seek transaction. Нельзя продолжать seek,
если старое состояние декодера не сброшено.

### Основные файлы

- `crates/player-core/src/session.rs`
- `crates/video-vaapi/src/decoder_thread.rs`
- `crates/player-core/src/error.rs` или ближайший typed error module.
- Tests в `crates/player-core/src/session.rs` или отдельном seek test module.

### Правки

- Вынести reset видео декодера в небольшую внутреннюю функцию session-level.
- Если `flush()` возвращает ошибку, завершать seek transaction как failed.
- Зафиксировать policy: либо `Paused + stale_frame = true + recoverable error`,
  либо recreate decoder backend. Это важное решение, перед ним остановиться и
  спросить.
- Не менять алгоритм preroll/drop в этой сессии.

### Тесты

- Тест с mock/fake decoder thread, где flush fail не вызывает demux seek.
- Тест, что после flush fail `seek_commit` очищен или переведён в fail-state
  согласно выбранной policy.
- Smoke: `cargo test -p player-core -p webm-demux -p app-egui`.

### Definition of Done

- Невозможно получить новую seek generation поверх декодера, который не смог
  сброситься.
- Ошибка не игнорируется молча и видна через diagnostics/snapshot.

## Сессия 3. Честный внутренний API для `SeekMode`

### Задача

Сделать так, чтобы `SeekMode` означал реальную политику, а не терялся при video
seek.

### Основные файлы

- `crates/player-core/src/command.rs`
- `crates/player-core/src/seek_state.rs`
- `crates/webm-demux/src/demuxer.rs`
- `crates/webm-demux/src/symphonia_demuxer.rs`
- `crates/webm-demux/src/dual_stream_demuxer.rs`

### Правки

- Определить контракт: какие modes реально поддерживает demuxer.
- Добавить explicit unsupported/capability path для `KeyframeAfter`, если он
  пока не реализуется.
- Убрать silent fallback, где video always превращается в `DecodePointBefore`.
- Обновить tests на mapping `SeekMode -> DemuxSeekRequest`.

### Стоп-решение

Перед реализацией спросить: `KeyframeAfter` нужен сейчас как hard requirement
или достаточно typed unsupported до отдельной реализации.

### Тесты

- Unit tests для `SeekCommitState::demux_seek_request_for_transaction`.
- Tests в `webm-demux` на Accurate/DecodePointBefore/Preview behavior.
- Проверить, что audio-only final seek остаётся accurate.

### Definition of Done

- Любой новый `SeekMode` либо реализован, либо явно отклонён typed ошибкой.
- Demuxer default не может молча игнорировать mode.

## Сессия 4. Generation token для scrub intent

Статус 2026-05-23: исторический пункт старого live preview plan. В текущем
runtime aggressive drag не создаёт preview/final intent sequence; release
коммитится как один normal final seek. Раздел ниже не использовать как active
prompt без новой архитектурной переписи live preview.

### Задача

Защитить preview/final seek от устаревших пользовательских intent при агрессивном
scrub.

### Основные файлы

- `crates/player-core/src/seek_controller.rs`
- `crates/player-core/src/command.rs`
- `crates/player-core/src/worker.rs`
- `crates/player-core/src/session.rs`

### Правки

- Ввести typed `ScrubGeneration` или `SeekIntentId`.
- Передавать id от `BeginScrub` через `UpdateScrub`, `PreviewScrub`, `EndScrub`.
- Игнорировать устаревшие preview/final команды до входа в `PlayerSession`.
- Разделить `latest_target` и настоящий `in_flight_target`: in-flight должен
  означать отправленную транзакцию, а не просто последний update.

### Тесты

- Fast sequence: begin A, preview A, begin B, delayed preview A не применяется.
- EndScrub для старой generation не коммитит новую timeline.
- Existing coalescer tests не теряют latest-wins behavior.

### Definition of Done

- В seek transaction всегда понятно, к какому пользовательскому scrub intent она
  относится.
- Устаревшие preview events не могут снять stale state новой операции.

## Сессия 5. Preview timeout и UI stale semantics

Статус 2026-05-23: исторический пункт старого live preview plan. Текущий active
contract не использует preview timeout как acceptance-критерий, потому что live
preview transaction удалён. Раздел ниже не использовать как active prompt без
новой архитектурной переписи live preview.

### Задача

Сделать UI состояние честным: если fresh preview не был показан, stale нельзя
снимать как будто seek завершился успешно.

### Основные файлы

- `crates/player-core/src/session.rs`
- `crates/media-core/src/time.rs`
- `crates/app-egui/src/ui/timeline.rs`
- `crates/app-egui/src/state.rs`

### Правки

- Пересмотреть `fail_preview_seek_commit_on_timeout`.
- Добавить явное состояние preview failed/expired, если текущего `stale_frame`
  недостаточно.
- Разделить Escape cancel и commit latest. Текущий
  `cancel_active_timeline_scrub` коммитит latest target.
- Не менять final seek gates в этой сессии.

### Стоп-решение

Спросить UX-policy:

- Escape отменяет scrub и возвращает позицию до drag;
- Escape коммитит текущий latest target;
- Escape коммитит последний видимый preview.

### Тесты

- Preview timeout без presented target frame не ставит timeline в "fresh" вид.
- Escape behavior покрыт UI action test.
- `lost_focus` не создаёт неожиданный cancel/commit без явной policy.

### Definition of Done

- Пользователь не видит "готовое" состояние, если кадр целевой позиции не был
  показан.
- Cancel и commit имеют разные имена и разные команды.

## Сессия 6. Seek-time frame admission без present backpressure

### Задача

Убрать риск, при котором seek ждёт целевой кадр, но decoder output не дренируется
из-за заполненной обычной present queue.

### Основные файлы

- `crates/player-core/src/tick.rs`
- `crates/player-core/src/session.rs`
- `crates/player-core/src/pipeline.rs`
- `crates/video-vaapi/src/decoder_thread.rs` только для проверки контракта.

### Правки

- Разделить normal playback admission и seek admission.
- Во время active seek дать decoder output дренироваться до target/preroll даже
  при нулевых обычных present slots.
- Проверить, что это не ломает render lease ownership и не создаёт unbounded
  queue.
- Сохранить backpressure для обычного playback.

### Тесты

- Synthetic test: present queue full, active seek, decoder publishes target frame,
  seek still can commit.
- Test на отсутствие unbounded growth при длинном preroll.
- Regression: paused video seek still показывает target frame.

### Definition of Done

- Seek progress не зависит от заполненности обычной playback present queue.
- Render lease contract не изменён.

## Сессия 7. EndScrub commit policy

Статус 2026-05-23: исторический пункт. Текущий drag release не выбирает
visible-preview policy; он временно коммитит latest pointer target через normal
final seek. Раздел ниже не использовать как active prompt без новой
архитектурной переписи live preview.

### Задача

Сделать политику завершения scrub явной и тестируемой.

### Основные файлы

- `crates/player-core/src/session.rs`
- `crates/player-core/src/seek_controller.rs`
- `crates/player-core/src/command.rs`
- `crates/app-egui/src/state.rs`

### Стоп-решение

Спросить перед кодом, какую policy выбираем по умолчанию:

- `CommitLatestTarget`: release всегда делает final seek в позицию курсора;
- `CommitVisiblePreview`: release фиксирует последний реально показанный preview;
- hybrid: если latest target не был previewed, сначала показать stale и сделать
  final exact seek.

### Правки

- Закодировать выбранную policy typed enum-ом.
- Обновить tests, которые сейчас ожидают сохранение last visible preview.
- Убрать неявную магию из `complete_visible_preview_seek_as_final`.

### Тесты

- Release immediately after update.
- Release while preview transaction active.
- Release after visible preview.

### Definition of Done

- Поведение release описано одним enum/policy, а не спрятано в ветках session.

### Исторический итог 7-й сессии, 2026-05-15

Статус на конец сессии был таким: перемотка стала рабочей для целевого UX - без
обычных frame drops, без залипания seek state и без заметной задержки при
отпускании timeline после уже показанного preview. Этот блок оставлен как
история принятого тогда решения; он не описывает текущий runtime после удаления
live preview core.

Выбранная по умолчанию policy для timeline release:

- `CommitVisiblePreview`.

Причина выбора изменилась по результатам ручной проверки. Изначально hybrid
казался предпочтительным, потому что он сохраняет last visible preview как
feedback и потом доезжает exact seek-ом до latest target. На реальном HDR/P010
4K60 WebM это дало сильную задержку на release: final exact seek попадал на
decode point за несколько секунд до requested target, и decoder должен был
пройти длинный preroll перед тем, как session могла закрыть final commit.

Важный нюанс: это была не проблема "магии" в `complete_visible_preview_seek_as_final`
и не простой баг coalescing-а. Гибридная policy работала как accuracy-first
policy и честно ждала target frame, но для timeline release это плохой UX,
потому что пользователь уже видел приемлемый preview. Поэтому default release
сделан latency-first: если кадр preview реально был показан, release фиксирует
его сразу и не запускает новый exact final seek.

Что осталось явно доступным:

- `CommitLatestTarget`: всегда финально ехать в latest target.
- `CommitVisiblePreview`: фиксировать последний реально показанный preview.
- `CommitLatestTargetWithVisiblePreviewFallback`: hybrid/exact policy для
  сценариев, где важнее exact target, чем мгновенный release.

### Текущий контракт seek semantics, 2026-05-23

Этот блок заменяет старый visible-preview release contract. Главная граница:
текущий pointer drag не создаёт live preview transaction, а release коммитит
latest target через normal final seek.

- Pointer timeline drag release: UI хранит transient pointer position локально,
  а на release отправляет simple final seek в latest pointer target.
- Compatibility `BeginScrub`/`UpdateScrub`/`PreviewScrub`/`EndScrub` API может
  сохраняться для публичной формы команд, но не должен стартовать live preview
  transaction или visible-preview promotion.
- Click-to-seek остаётся active final seek сценарием. Он не должен зависеть от
  last visible preview или timeline release policy.
- Keyboard seek, External/MPRIS seek и future chapter seek используют
  `PlayerCommand::Seek(SeekRequest)` как exact/final route.

Зафиксированные текущие edge cases:

- Release immediately after update: normal final seek идёт в latest release
  target без preview precondition.
- Release while old preview transaction would have been active: такого active
  preview transaction в текущем runtime быть не должно.
- Release after drag movement: acceptance проверяет demux accepted, packets,
  decoded/presented frame для video media и `Final seek commit завершён`.

Если будущая live preview rewrite вернёт S5/S6, она должна добавить новый явный
contract, diagnostics markers и tests отдельным архитектурным изменением.

## Сессия 8. Codec/backend readiness boundary

### Задача

Подготовить seek к нескольким video/audio codecs без добавления нового codec в
этой сессии.

### Основные файлы

- `crates/player-core/src/pipeline.rs`
- `crates/player-core/src/session.rs`
- `crates/video-core` или ближайший crate с decode contracts.
- `crates/video-vaapi/src/decoder.rs`
- `crates/media-core` capability types.

### Правки

- Проверить, где `OpusDecoder` и `VaapiVideoDecoder` просачиваются в player
  orchestration.
- Ввести минимальные trait/enum boundaries только там, где seek/reset зависит от
  codec-specific типа.
- Не добавлять AV1/MP4/fMP4 в этой сессии.
- Не переименовывать крупные factory без отдельного grep-а по call sites.

### Тесты

- Unit tests на generic reset boundary.
- Existing VP9/Opus path должен остаться без изменений поведения.

### Definition of Done

- Seek/reset код зависит от "audio decoder reset" и "video decoder reset", а не
  от конкретного Opus/VP9 типа в orchestration layer.

## Сессия 9. Stress tests и ручная матрица проверки

### Задача

Закрепить текущий final-seek behavior тестами, которые имитируют aggressive
drag/release без live preview transaction.

### Основные файлы

- `crates/player-core/src/session.rs`
- `crates/player-core/src/worker.rs`
- `crates/player-core/src/tick.rs`
- `crates/app-egui/src/ui/timeline.rs`

### Automated tests

- 1000 drag updates, затем release commit как normal final seek.
- Compatibility preview commands сохраняют latest target без demux preview seek.
- EndScrub без latest target очищает lightweight scrub state без seek-а.
- Flush fail во время final seek.
- Present queue full во время seek.
- EOF fallback после seek около конца файла.
- Audio+video final seek waits for selected gates only.
- Audio-only accurate seek не ломается после изменений video seek.

### Manual verification

- Local VP9 SDR WebM.
- Local VP9 HDR/P010 WebM.
- YouTube VOD через текущий service path.
- Drag timeline медленно: до release виден только transient UI state, на release
  выполняется final seek.
- Drag timeline рывками: worker не получает live preview stream, на release
  выполняется один latest-target final seek.
- Click-to-seek.
- Release immediately after drag.
- Escape during drag according to chosen policy.
- Seek near EOF.
- Seek while playing and while paused.

### Definition of Done

- В diagnostics нет обычных playback/render drops после seek.
- Seek-discard виден отдельно и не считается проблемой плавности.
- UI не зависает в stale или false-fresh состоянии и не обещает live preview.
- Все session-level и smoke tests проходят.

## Рекомендуемый порядок

1. Сессия 1: сначала очистить telemetry, чтобы видеть реальные проблемы.
2. Сессия 2: закрыть опасную fail-open границу flush.
3. Сессия 3: выровнять внутренний API seek mode.
4. Сессия 4: защитить intent generation.
5. Сессия 5: исправить stale/timeout/UI semantics.
6. Сессия 6: убрать seek-time backpressure.
7. Сессия 7: явно выбрать EndScrub policy.
8. Сессия 8: подготовить codec/backend boundary.
9. Сессия 9: stress verification.

## Copy-paste шаблон для новой сессии

```text
Работаем в <REPO_ROOT>.
Используй docs/rustiplayer/12-seek-reliability-plan.md как план.
Начни с code_index set_project_path + build_deep_index.
Перед правками сверь релевантные внешние API с Context7.

В этой сессии делаем только: <номер и название сессии>.
Не трогай код вне перечисленных файлов без явного объяснения.
Если нужно принять policy/architecture решение, остановись и спроси.
После реализации запусти targeted tests и:
cargo test -p player-core -p webm-demux -p app-egui
В финале дай self-review и список оставшихся рисков.
```
