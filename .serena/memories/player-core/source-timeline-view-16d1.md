# Табло шкалы источника: эфир / запись / «пока неизвестно» (мост 16d1, 2026-11-13, решения 318–322)

Продолжение `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`: тот же port теперь несёт не только эфир.

## Контракт (`media-core/src/dynamic_timeline/view.rs`)
- `DynamicMediaTimelineSnapshot.state` / `DynamicMediaTimelineInitial.state` — `DynamicMediaTimelineView::{Live(DynamicMediaTimelineState), Recording(RecordingTimelineState), Undetermined}`; `as_live()`, `as_recording()`, `kind()`; `From<DynamicMediaTimelineState>` — источники эфира пишут `.into()`.
- `RecordingTimelineState::{from_start(duration), ending_at(end, available_range)}` — край (длительность) отдельно от диапазона перемотки (решение 325: законченный HLS-эфир — край окончательный, перемотка по прочитанному; начало может быть не ноль); пустой → `EmptyRecordingRange`, диапазон за краем → `RecordingRangeBeyondEnd`. Ядро: `duration = end`, `seekable_range = available_range`; Static-режим зажимает цель в `seekable_range` (и точная перемотка MPRIS — через `seek` → `resolve_seek_target`), тест `recording_timeline::an_exact_seek_past_what_was_read_lands_at_the_read_edge_instead_of_failing`.
- Происхождение (решение 328): `DynamicMediaTimelinePort::opened_as()` — вид табло при создании (не меняется); `PlayerSnapshot.timeline_origin: TimelineOrigin::{Recording, Live}` (`player-core/src/timeline_origin.rs`, `opened_as_live()`): ставится при установке по `opened_as` и в `apply_live_view`, сбрасывается в `set_timeline_duration`/`clear_timeline`. Приложение по нему (а не по `TimelineMode`) не пишет место/точку сна: `resume_persistence.rs`, `suspend_resume.rs`, `lifecycle_settlement.rs`, `strong_media_open(.rs, /pending/resume.rs)`. Время эфира, ставшего записью, отсчитано от окна эфира — при новом открытии законченного плейлиста отсчёт другой.
- HLS: `web-media-hls/src/live/timeline_publication.rs` — ENDLIST → запись (`mem:browser-bridge/core`, 16d1).
- `publish(epoch, live_state)` — прежний вход эфира; `publish_view(epoch, view)` проверяет переход (`check_view_transition`): разрешены Undetermined→Recording, Live→Live|Recording, Recording→Recording; запрещены переходы В эфир из другого вида и возврат в Undetermined → `DynamicMediaTimelinePublishError::ForbiddenViewTransition{from,to}`, snapshot/revision не меняются. Эфир решается при открытии.

## Ядро (`player-core`)
- `PreparedMediaTimelineMode::Dynamic{port}` (было `Live`). `with_dynamic_timeline` берёт длину из табло (запись → край, иначе None), ошибки «эфиру нужна неизвестная длина» больше нет. `PreparedMedia::initial_timeline_mode()`, `staged_live_timeline_state()` (только вид эфира).
- `session/dynamic_timeline.rs` `apply_dynamic_timeline_snapshot` ветвит по виду: `apply_live_view` (прежняя S31L) / `recording_view.rs` `apply_recording_view` (Static, длина, seekable = доступный диапазон, ЧАСТИЧНОЕ обновление — цель перемотки и флаги не сносятся; лог «Live timeline became a recording» один раз) / `apply_undetermined_view`. Установка доверяет свежему снимку (гонка подготовка↔установка закрыта тестом).
- Один писатель шкалы: `apply_demuxer_reported_duration` (смена дорожек) не пишет длину ни у записи, ни у эфира с табло (с 13.11, решение 322; тест `recording_timeline::a_track_list_update_keeps_the_live_window`). Цепочка эфира, на которой держался `live_page_restart`, разобрана: правило «звук ждёт своего PTS» и у эфира (`mem:player-core/audio-runtime`), эстафета вместо сброса посреди перемотки (`mem:player-core/track-description-handoff-03d`, решение 323), защёлка программного декода в окне (`mem:app-egui/dynamic-decode-policy`, решение 324). Процессный судья `browser_session_process/live_page_restart.rs`: часы без прыжков вперёд, кадры и звук по итоговой позиции, запусков декодера ≤ 3.
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
