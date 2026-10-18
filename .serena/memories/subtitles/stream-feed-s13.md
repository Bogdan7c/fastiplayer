# Лента субтитров потоковых источников (сессия 13, пакет P4, L3)

Крейт `media-source-open`, модуль `stream_subtitles/` (pub). Связано: `mem:media-services/hls-subtitle-transport-s13-2026-10-18` (L2 HLS), `mem:subtitles/web-tracks-s12`, `mem:media-source-open/core`.

## Слои и файлы
- `model.rs`: `StreamSubtitleTrackId(u32)`, `StreamSubtitleTrackInfo{id,language,name,forced,default,autoselect}`, `StreamSubtitleSource` (Clone; Eq по `Arc::ptr_eq` транспорта; Debug без URL).
- `transport.rs`: трейт `StreamSubtitleTransport: Send+Sync` (`tracks`, `load_index`, `fetch_and_decode`), `StreamSegmentIndex{segments,refresh: Never|After}`, `StreamSegment{key,reference(opaque),start,duration}`, `SegmentDecode::{Ready(DecodedSegment), Pending(Box<dyn PendingSegmentDecode>)}`. События ВСЕГДА на шкале плеера; `DecodedSegment.segment_start` — точная привязка начала сегмента (из неё лента учит смещение плейлист→плеер для live). DASH (P6) реализует тот же трейт.
- `error.rs`: `StreamSubtitleError` (UnknownTrack/NotFound/Network/Rejected/TooLarge/Malformed/Unsupported/Cancelled/Stale), `is_transient()` (=Network), `is_cancellation()`, `failure_kind()`.
- `hls/`: `HlsStreamSubtitleTransport::new(HlsSubtitleTransport, HlsTimestampOrigins).into_source()`; `decode.rs` (VTT разбор через `load_text_subtitle`, привязка), `timing.rs` (чистая арифметика, разворот 33 бит).
- `feed/`: `StreamSubtitleFeed::start(&source, track, StreamSubtitleFeedConfig, StreamPlaybackPosition, wake)`; `try_next_message/drain_messages/cancel/track`; `Drop` = cancel + wake, поток НЕ join-ится. `config.rs` (без default; ahead, behind, max_segment_bytes, max_pending_segments, failure_threshold, message_capacity), `position.rs` (`advance_to` не будит, `jump_to` будит condvar), `schedule.rs` (чистый планировщик), `worker.rs` (поток `stream-subtitle-feed`), `message.rs`.

## Тайминг HLS (инварианты)
- С `X-TIMESTAMP-MAP`: `media = MPEGTS(развёрнут к ближайшему к media_origin эпохи)/90k + (cue − LOCAL)`; `player = media − media_origin + timeline_start`. Ключ эпохи = `discontinuity_sequence` сегмента плейлиста субтитров. Live: пара по `origin_for_media_time(d, media)` (одна на сегмент), иначе самая ранняя пара эпохи.
- Без карты, VOD (как hls.js): время реплик отсчитывается от начала эпохи разрыва → `player = cue + timeline_start(эпохи)`; `segment_start=None`.
- Без карты, LIVE (ревью F1): RFC 8216 §3.5, нет карты = `MPEGTS:0↔LOCAL:0`, время реплики = время контейнера; по самой ранней реплике сегмента берётся разворот 33 бит (`timing::wrap_shift_near`) и пара `origin_for_media_time`; `player = cue + wrap − media_origin + timeline_start`. `segment_start=None` → live DVR назад старые сегменты не подтягивает (окно — хвост у края). Вид плейлиста несёт `SegmentFacts.playlist_kind` (`PlaylistKind::{Vod,Live}`).
- Стабильная шкала: `StreamSegment.start` стабилен между перечитываниями (держит `HlsSubtitleTransport`, см. L2-память). Трейт получил метод с реализацией по умолчанию `index_timeline_epoch(track) -> StreamTimelineEpoch` (HLS отдаёт поколение шкалы последнего `load_index`; DASH — всегда дефолт). Смена поколения → лента забывает `Learned`-смещение (→ `Unknown`, хвост у края).
- Точка отсчёта эпохи ещё не опубликована демуксером → `Pending`; сегмент хранится разобранным (без повторной сети), досчитывается каждым проходом цикла; список ограничен `max_pending_segments` (вытеснение старейшего → `SegmentSkipped{PendingOverflow}`).
- Ограничение: live > ~13 ч в одной эпохе — старые пары вытесняются (см. память L2).

## Политика ленты
- VOD: смещение плейлист↔плеер = Identity; окно `[pos−behind, pos+ahead]`, порядок: от позиции вперёд, затем назад от ближайшего; обработанные (загруженные ИЛИ пропущенные) не повторяются, пока лежат в окне. Вышедшие из окна `handled` забываются каждым проходом (`forget_handled_outside_window`, `schedule::is_in_window`; при `Unknown`-смещении не трогаем) → перемотка назад за срок хранения приложения скачивает их снова (дубли снимает шкала приложения).
- Ожидающий (Pending) сегмент НЕ сбрасывает серию сбоев; успех засчитывается при реальной привязке. Вытеснение (`PendingOverflow`) и уход из плейлиста (`LeftPlaylist`) = `abandon_pending`: убрать из `handled` (снова скачиваем) и посчитать сбоем ТОЛЬКО если воспроизведение уже прошло конец сегмента (`playback_has_passed`; при `Unknown`-смещении не прошло) → постоянное расхождение кончается ровно одним `TrackUnavailable`, а окно вперёд, зашедшее в эпоху после разрыва (видео там ещё не было), здоровый поток не гасит. Зонд эпохи: пока есть ожидающий сегмент, не скачиваем сегменты не раньше самого раннего ожидающего, до которых позиция не дошла (`keys_waiting_behind_pending`) — без переполнения и холостых запросов.
- Live: пока смещение неизвестно — «хвост» (ahead+behind у живого края, новые первыми); после первой точной привязки окно по позиции (DVR назад работает). Индекс перечитывается по `After(target_duration, ≥1 с)`; `handled` чистится по ключам индекса.
- Сбои: сегмент после HTTP-повторов → `SegmentSkipped{Failed(kind)}`; порог подряд → ровно одно `TrackUnavailable(ConsecutiveSegmentFailures{count})` и остановка; первая загрузка индекса не удалась (или live-перечитывание порог раз подряд) → `TrackUnavailable(IndexLoadFailed(kind))`. `Recovered` — после успеха, идущего за пропуском. Отмена транспорта (`Cancelled`) завершает ленту молча.
- Ожидание: condvar по `wake_epoch` с таймаутом (половина самого длинного сегмента, ≥250 мс, ≤ahead; при pending — ¼ этого, 100 мс..1 с). Полная очередь сообщений — ждёт по 20 мс, слыша отмену.

## Проводка
`PreparedNativeHlsVod/Live.stream_subtitles` → `PreparedNativeHlsRuntime::into_parts` (4-й элемент) → `NativeHlsSourceState::with_stream_subtitle_source / stream_subtitle_source()`. yt-dlp HLS: `PreparedHlsCandidate.stream_subtitles` → `OpenedWebCandidate.stream_subtitles` → `ExtractorMediaSourceState::stream_subtitle_source()`. Транспорт и origins берутся ДО `into_demuxer/into_parts`. Каждое открытие/reopen даёт свежий источник. App: `WebMediaSourceIntent::stream_subtitle_source()` (`media_open/web/source_actions.rs`, NativeHls+Extractor); потребитель — владелец субтитров, см. `mem:subtitles/stream-tracks-s13`.

## Тесты
- unit: `hls/timing_tests.rs` (33-битный оборот), `feed/schedule_tests.rs`, `tests/` (подставной транспорт: окна, seek+wake, сбои/порог/индекс, pending, live, drop при зависшем запросе, Debug).
- интеграция `crates/media-source-open/tests/stream_subtitle_hls.rs` (общий `support` web-media-hls через `#[path]`, `cargo-machete ignored = symphonia-format-isomp4`): реальное открытие HLS, реплика = pts демуксированного видеопакета + смещение (допуск 3 мкс), LOCAL≠0, discontinuity, без карты, 404/порог/плейлист-мусор, seek по реальным запросам, live reload, Debug без секретов.
- `tests/feed_timeline.rs` (DVR после скользящих перечитываний + смена поколения шкалы), `tests/stream_subtitle_hls_live.rs` (реальный live: нет карты → время контейнера; `load_index` стабильные start + разрыв).
- app: `media_open::web::tests::native_hls_intent_hands_out_the_subtitle_source_of_its_own_open`.

## Проверки
`cargo test -p media-source-open -p web-media-hls --locked`, `cargo clippy -p media-source-open -p app-egui --all-targets --locked -- -D warnings`, `cargo fmt --all --check`, `scripts/check-refactor-guardrails.py`, `scripts/check_s42_guardrails.py`, `cargo machete --with-metadata crates/media-source-open`.

## DASH (пакет P6)
`stream_subtitles/dash/` (`DashStreamSubtitleTransport`, `decode.rs`) реализует тот же трейт поверх `web_media_dash::DashTextSubtitles`; события приходят уже со сдвигом на шкалу плеера, `StreamSegment.start` DASH — шкала плеера (смещение 0). Проводка: `NativeDashSourceState::stream_subtitle_source()` (VOD+live), app-арм `NativeDash` в `stream_subtitle_source()`. Подробности: `mem:subtitles/dash-text-tracks-s13`.
