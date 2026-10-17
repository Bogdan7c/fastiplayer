# Поток-рисовальщик субтитров (сессия 09, этап 1, 2026-10-17; решение владельца: отдельный поток сразу + синхронность при seek/scrub/любой скорости; промах = старая картинка без ушедших реплик)

UI-поток НИКОГДА не вызывает `SubtitleRasterizer` и не ждёт рисовальщика. Код: `crates/app-egui/src/state/subtitles/render_worker/` + `presentation/{presenter,worker_link,test_support}.rs`, `owner_render.rs`.

## Части
- `worker.rs` (поток `subtitle-render`, цикл: забрать все команды -> `plan_next_render` -> одно время -> publish; спит на `recv`), `render_step.rs`, `track_mirror.rs` (зеркала `source_id -> timeline`), `published_record.rs` (что поток уже выдал; ёмкость = max(`prepared_overlays`, `prerender_frames`+1)).
- `handle.rs` (`RenderWorkerHandle`: spawn/send/drain_events/request_shutdown; Drop = Shutdown+join), `commands.rs` (WorkerCommand/WorkerEvent), каналы `std::sync::mpsc` + `wake_port.request_wake()` после send.
- UI: `SubtitlePresenter` (фасад), `PreparedQueue` (select Exact/StaleWithoutEnded/Nothing), `MirrorSync` (сверка пары `source_id + timeline.len()` на каждом кадре -> Replace/Append(дельта по id)/Forget; страховка полный Replace), `ClockTracker` (наблюдаемый шаг pts соседних РАЗНЫХ кадров, <=1 с, иначе оценка снимка), `LatenessMonitor` (warn только при >=24 опозданий в окне 240 кадров).
- `Present{target, clock, look}` — входы рисования (`LookInputs`, ревизия) едут в одной команде с часами (иначе гонка Look/Present даёт лишнюю растеризацию).
- Late append: UI сразу инвалидирует очередь по отрезку новых реплик; поток выбрасывает задетые интервалы своей записи и перерисовывает; запоздавшие картинки отсекаются по `mirror_serial` (см. «Правки после ревью»).
- Режим seek/scrub: `AppState::observe_playback_for_subtitles()` вызывается в `frame_prepare/submit.rs` на КАЖДОМ кадре (даже без видеокадра); цель -> `refresh_target_clock`. Во время scrub кадры превью несут identity (stale_frame=false) — субтитры следуют за показанным кадром; stale-lease при seek без identity — субтитры скрыты.
- Отказ потока: `RenderWorkerFailure` -> `SubtitleLoadFailure::{RenderWorkerStartFailed, RenderWorkerLost}` -> показываемая дорожка `Unavailable`.
- Опрос потока — ПОСЛЕДНИМ в `SubtitleOwner::poll_jobs` (остальные опросы сбрасывают edge wake-порта).

## Правки после ревью
- Единое опорное время: `PresentationClock::planning_anchor()` (цель в seek/scrub, иначе кадр) и `planning_window(horizon)`; запись потока (`retain_window`) и очередь UI (`note_planning_anchor`) выбрасывают «ушедшее» от него. Защита потока: то же (identity, время) дважды подряд без команды не рисуется (`repeated_jobs_suppressed`).
- Размер очереди/записи = max(`prepared_overlays`, `prerender_frames`+1) (горизонт не зажимается).
- Изменения дорожек (`Replace`/`Append`) несут `serial`; картинки помечены `mirror_serial`; UI хранит недавние изменения (`ingest.rs`) и отбрасывает картинки, нарисованные до задевающего их изменения. События `SpanInvalidated` больше нет.
- `Present` и команды шрифтов несут `LookInputs`; ожидаемая цель сверяется с ЖИВОЙ `inputs_revision`. Смена media сбрасывает холст/ASS-опции/шаг кадров (`forget_playback_context`).

## Инварианты
- Идентичность картинки = source + canvas + inputs revision (растёт: стиль, ASS-опции/размер картинки, шрифты, `forget_shown_overlay`). `forget_shown` чистит очередь; смена sync offset НЕ инвалидирует (картинки во времени субтитров).
- Фоллбэк не берёт чужой source; закончившиеся реплики режутся (`retain_cues`).
- Известное ограничение: в режиме правки часы идут только для выбранной дорожки (primary или sample) — первый кадр primary-реплики может быть опоздавшим. Память коллизий ASS растеризатора зависит от порядка рисования (prerender вперёд).

## Тесты
`state/subtitles/tests/render_thread.rs` (реальный поток), по умолчанию остальные тесты работают в `PumpMode::SettleBeforeSelect` (презентатор ждёт idle потока); `FreeRunning` — асинхронное поведение. Тестовые хуки: `Flush`, `SetRenderDelay`, `QueryStatistics`, `SimulateCrash`.

Приёмка вживую (агент, Wayland+X11): uinput-драйвер (клавиши + абсолютный указатель), отдельный `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_STATE_HOME`; у `12_quick_check` автовыбор берёт встроенную дорожку языка системы — для сдвинутой SRT выбрать файл в меню CC. Поток `subtitle-render` ~0 % CPU в игре.

Конфиг v19 `[subtitles]`: `prerender_frames` (4, 0..=64), `prepared_overlays` (8, 1..=64); действуют с созданием нового потока (следующее media).
