# Табло шкалы источника: эфир / запись / «пока неизвестно» (мост 16d1, 2026-11-13, решения 318–322)

Продолжение `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`: тот же port теперь несёт не только эфир.

## Контракт (`media-core/src/dynamic_timeline/view.rs`)
- `DynamicMediaTimelineSnapshot.state` / `DynamicMediaTimelineInitial.state` — `DynamicMediaTimelineView::{Live(DynamicMediaTimelineState), Recording(RecordingTimelineState), Undetermined}`; `as_live()`, `as_recording()`, `kind()`; `From<DynamicMediaTimelineState>` — источники эфира пишут `.into()`.
- `RecordingTimelineState::{from_start(duration), with_available_range(range)}` — край = длительность, начало может быть не ноль (скользящий HLS с ENDLIST); пустой → `EmptyRecordingRange`.
- `publish(epoch, live_state)` — прежний вход эфира; `publish_view(epoch, view)` проверяет переход (`check_view_transition`): разрешены Undetermined→Recording, Live→Live|Recording, Recording→Recording; запрещены переходы В эфир из другого вида и возврат в Undetermined → `DynamicMediaTimelinePublishError::ForbiddenViewTransition{from,to}`, snapshot/revision не меняются. Эфир решается при открытии.

## Ядро (`player-core`)
- `PreparedMediaTimelineMode::Dynamic{port}` (было `Live`). `with_dynamic_timeline` берёт длину из табло (запись → край, иначе None), ошибки «эфиру нужна неизвестная длина» больше нет. `PreparedMedia::initial_timeline_mode()`, `staged_live_timeline_state()` (только вид эфира).
- `session/dynamic_timeline.rs` `apply_dynamic_timeline_snapshot` ветвит по виду: `apply_live_view` (прежняя S31L) / `recording_view.rs` `apply_recording_view` (Static, длина, seekable = доступный диапазон, ЧАСТИЧНОЕ обновление — цель перемотки и флаги не сносятся; лог «Live timeline became a recording» один раз) / `apply_undetermined_view`. Установка доверяет свежему снимку (гонка подготовка↔установка закрыта тестом).
- Один писатель шкалы: `apply_demuxer_reported_duration` (смена дорожек) не пишет длину у ЗАПИСИ с табло. **У эфира пока по-старому** — затирает шкалу эфира; на этом держится `live_page_restart` (09b2): правило «звук ждёт своего PTS» (`audio_gap/scheduled_audio_start.rs`) только Static. Чинится в 16d1 по решению 322 (общий бэклог п. 51), тест `recording_timeline::a_track_list_update_keeps_the_live_window` ignored до починки.
- Тесты: `session/tests/recording_timeline.rs` (эфир→запись, рост без срезания перемотки, один писатель, гонка установки, рост во время перемотки в конец).

## Приложение
- `state/timeline_wake_identity.rs`: будильник шкалы сравнивает media, режим, ревизию эфира, длину и диапазон перемотки (у записи ревизии эфира нет).
- Окно браузерного режима, `browser_session/window/desktop_transport.rs`: прыжок позиции (назад или вперёд дальше прошедшего времени + 250 мс, `position_jumped`) публикуется в MPRIS сразу; плавный ход — не чаще 4 Гц. Причина: полная публикация (смена длины записи) за миг до приземления перемотки держала старую позицию (`page_variants` dub switch).

## Источник MSE (`pushed-media-source`)
- Табло у эфира и у каждого потока, где все полосы с метками времени (`OpenedPushedStream.timeline`, `opened_as_live()`); вид — `timeline_view.rs` `view_for`/`next_view`. `PushedStreamWriter::set_duration` публикует сразу (`StreamBuffer::note_declared_duration`).
- Край записи = max(объявленная, самый дальний конец принятого сегмента `BufferState.furthest_timed_end`, O(1)); обход полки (`live_view`) — только у эфира (`LiveTimelineFeed::follows_the_shelf`). Обход на каждом сегменте душил приём на тысячах фрагментов (`journal_burst`).
- Отказы публикации, кроме `ConsumerDisconnected`, ленту не роняют (warn).

## Экстеншен
- Правило 319 «длиннее года = эфир»: `extensions/browser/src/shared/endless-duration.ts` (`ENDLESS_DURATION_THRESHOLD_SECONDS`, `isEndlessDurationSeconds`) — в `page/mse-values.ts` `durationOf`, `page/hls-stream-kind.ts`, `page/candidate-facts.ts`, `page/page-return-step.ts`. Twitch в Firefox: элемент 9 223 372 036 854,775 с.

Связи: `mem:pushed-media-source/core`, `mem:browser-bridge/core` (раздел 16d1), `mem:player-core/audio-runtime`.
