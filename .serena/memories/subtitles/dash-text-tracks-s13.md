# Субтитры DASH: текстовые AdaptationSet (сессия 13, пакет P6)

Связано: `mem:subtitles/stream-feed-s13` (лента, трейт `StreamSubtitleTransport`), `mem:subtitles/stream-tracks-s13` (владелец в app), `mem:media-services/native-dash-vod-n09-2026-09-01`, `mem:media-services/native-dash-live-n10-2026-09-01`, `mem:xml/core`.

## Слои
- `dash-mpd-core` (L1): текстовые наборы НЕ входят в `DashPeriod.adaptation_sets` (A/V-каталог не тронут). Отдельно `DashPeriod.text_adaptation_sets: Box<[DashTextAdaptationSet]>` + `skipped_text_adaptation_set_count` (`text_model.rs`: `DashTextKind::{SidecarTtml,SidecarWebVtt,SegmentedStpp,SegmentedWvtt}`, `DashTextRole`, `DashTextRepresentation{id,bandwidth,language,label,kind,base_url,addressing}`). Разбор — `parser/text_adaptation.rs`: поддерево набора вычитывается в буфер (`EventCursor.replay`), затем разбирается из буфера; ЛЮБАЯ ошибка содержимого набора = `Skipped` (счётчик), фатальны только ошибки самого XML. Обнаружение по атрибутам самого набора: `contentType="text"` | mime `application/ttml+xml`/`text/vtt` | `application/mp4` (или без mime) + codecs `stpp*`/`wvtt`; набор без признаков на своём уровне (mime пуст/`application/mp4`, не video/audio) читается в буфер и считается текстом, только если ВСЕ его Representation с текстовыми доказательствами (смешанный → A/V). DRM (`ContentProtection`) в наборе/Representation → пропуск набора. Period только из текста по-прежнему `MalformedSchema` (нет A/V). `parser.rs` разгружен: `parser/period.rs` (ParsedPeriod, parse_period, finalize_periods).
- `web-media-dash` (L2): `plan/text.rs` (`plan_text_period`: те же `plan_template/plan_list/resolve_optional_base`, правила «живого снимка» — частичное покрытие Period допустимо; `plan_template` теперь принимает `(id, bandwidth)`, A/V-поведение не менялось), `text/tracks.rs` (группировка по (язык, Label, forced, ordinal) через Period; имена: Label | "Captions" | "Forced" | "Subtitles", дубликаты " (2)"; default = роль main, иначе первая не-forced), `text/fetch.rs` (индекс + скачивание), `text.rs` (публичное API `DashTextSubtitles`: `tracks/load_index/fetch_segment`, `DashTextError`, `DashTextRefresh`), `text_fmp4/` (собственный читатель: `moov` → timescale/stsd/trex; `moof/traf/tfhd/tfdt/trun`+`mdat` → `TextSample`; `wvtt` → текст WebVTT; ограничен `MAX_SAMPLES_PER_SEGMENT`, типизированный `TextFmp4Error`).
- `media-source-open::stream_subtitles::dash` (L3): `DashStreamSubtitleTransport::new(DashTextSubtitles).into_source()`; `decode.rs` разбирает документы `load_text_subtitle` (Ttml/WebVtt) и сдвигает события `shifted_by`. `StreamSegment.start` у DASH УЖЕ на шкале плеера, поэтому `segment_start = Some(start)` (лента учит смещение 0).

## Тайминг (инвариант: та же шкала, что у видео)
- player = presentation − `source_origin`; `source_origin` = 0 (VOD) | `DashLiveSessionTimeline.source_origin()` (live, только чтение, `live/runtime/text_source.rs`).
- Базисы (`DashTextTimeBasis`): `PeriodTimeline` (отдельные TTML/VTT: время от начала Period); `MediaTimeline{pto}` (fMP4 + SegmentTemplate/файл целиком: media − PTO + начало Period, как `globalize_packet_timestamp`); `NormalizedAtSegmentStart` (fMP4 + SegmentList без PTO: tfdt первого сэмпла = начало сегмента). TTML внутри `stpp` — на media timeline дорожки (ISO 14496-30), не от начала сэмпла.
- Сдвиг (`player_shift_micros`, i64) считает L2, события сдвигает L3. `wvtt` → VTT с метками media timeline (мс, округление вниз).

## Живой MPD
- `DashLiveSnapshot.text: Arc<DashTextPlanSet>` строится в `build_dash_live_snapshot_with_selection` (вместе со снимком A/V, при каждом принятом обновлении). `DashLiveOpenResult::text_subtitles()` (до `into_parts`) читает последний снимок + актуальные http/generation одним lock; индекс перечитывается по `minimumUpdatePeriod` (≥1 с). Идентичность дорожки между снимками — `DashTextTrackIdentity`; список дорожек фиксируется по первому снимку. Источник субтитров держит `Arc<DashLiveShared>` (отпускать до проверки закрытия в тестах).
- Ограничение: при ротации токена/CDN у упавшего live-сегмента текста нет re-addressing (сегмент пропускается).

## Проводка
`DashVodOpenResult::text_subtitles()` (prepare_dash_vod и prepare_discovered_dash_vod) / `DashLiveOpenResult::text_subtitles()` → `native_startup/dash.rs` и `native_startup/dash/live_runtime.rs` → `NativeDashSourceState::with_stream_subtitle_source/stream_subtitle_source()` → app `WebMediaSourceIntent::stream_subtitle_source()` (арм `NativeDash`). yt-dlp DASH не подключён (манифесты yt-dlp без текстовых наборов): follow-up, если понадобится.

## Тесты
- dash-mpd-core: `tests/text_adaptation.rs` (4 вида, роли, DRM, счётчик, A/V неизменен, dynamic). web-media-dash: `text_fmp4/tests.rs`, `text/tests.rs`, `tests/live_runtime/text_subtitles.rs` (live-обновление индекса). media-source-open: `stream_subtitles/dash/tests.rs`, интеграция `tests/stream_subtitle_dash.rs` (настоящий `prepare_dash_vod`: sidecar TTML, IMSC top region, stpp+PTO во втором Period, wvtt+sttg, SegmentList-нормализация, файл целиком, 404 → пропуск, DRM → нет источника). app: `media_open/web/tests/stream_subtitles_dash_vertical.rs` (фикстура `open_dash_with_subtitles`, intent+лента), GPU-пиксели `state/subtitles/tests/stream_dash_gpu.rs` (фикстура открытия в media_open/web/tests).

## Guardrail
Читатель `text_fmp4` — осознанное узкое исключение в `scripts/s42_f4f_guardrail.py` (`TEXT_TRACK_FMP4_READER_ROOT`): правило «generic fMP4 parsing у symphonia-format-isomp4 patch» к нему не применяется; патч не менялся. Расширять читатель до A/V-демуксера нельзя.

## Исправления ревью (пакет F2)
- `text_fmp4/fragment.rs`: `SampleLayout` - диапазоны сэмплов строго вперёд без пересечений (`OverlappingSamples`), сумма байтов <= длина ресурса (`SamplesExceedSegment`); `trex` читает и `default_sample_size` (`MissingSampleSize`, если нигде нет).
- TTML: потолок времени снят до границы `i64` мкс (эпоха ~1.7e9 с проходит); WebVTT: часы до 6 цифр (до ~2084 г.). Сдвиг на шкалу плеера - checked, переполнение = `Malformed`.
- `NormalizedAtSegmentStart`: смещение считает ПЕРВЫЙ скачанный сегмент Period и кэширует в `TextShared.normalizations` (ключ `PeriodKey`), остальные его берут (как `NormalizeAtFirstPacket`).
- stpp: `DashTextDocument.sample_interval` (мкс media timeline) -> L3 `load_ttml_sample` + `TtmlSampleWindow` (subtitle-formats): обрезка окном сэмпла, открытый абзац до конца сэмпла. Для sidecar открытый абзац идёт до начала следующего абзаца, иначе до конца документа, иначе 5 с.
- Кэш init: `InitCacheKey{period, initialization_fingerprint}` (хеш id Representation + адрес init + range; сам адрес не хранится).
- `index_segments`: при упоре начала в 0 длительность укорачивается на обрезанную часть.
- Известное ограничение: сегментированный sidecar TTML/VTT (Template) с PTO читается Period-относительно.

## Не поддержано
`ctim` у wvtt; нефрагментированный MP4; plain-text сегменты (TTML/VTT по Template) с PTO считаются Period-относительными.

## Изоляция от видео-recovery (2026-10-18)
Статический и live источники снимков (`StaticTextSource`, `live/runtime/text_source.rs`) отдают `http.for_auxiliary_resources()`: сбои текстовых сегментов не доходят до endpoint-expiry наблюдателя видео. Тест `text::isolation_tests`.
