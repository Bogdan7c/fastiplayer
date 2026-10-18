# Субтитры: subtitle-core + subtitle-formats (сессия 01, 2026-10-14)

План: `user/subtitles/` (README — решения владельца, архитектура; отчёты `results/NN.md`). Матрица совместимости: `docs/subtitles/compatibility.md`.

## Границы
- `crates/subtitle-core` — нейтральная модель, deps только `oxilangtag`, `thiserror`. `crates/subtitle-formats` — разбор текстовых форматов, deps только `subtitle-core`, `encoding_rs`, `chardetng`, `thiserror`. Оба в `CONTRACT_CRATES` + строгие allowlist-ы `SUBTITLE_*_ALLOWED_DEPENDENCIES` в `scripts/check-refactor-guardrails.py` (тест `test_subtitle_core_rejects_gpu_dependency`). Никаких wgpu/egui/winit/render-*/player-core/media-core.
- Время — собственный `SubtitleTime` (i64 мкс), связь с pts плеера — в сессии 03. `SubtitleSyncOffset`: положительный = субтитры позже; `lookup_time_for(playback) = playback - offset`.

## Модель (subtitle-core)
- Дорожка: `SubtitleTrackDescriptor` (id, `SubtitleTrackOrigin` Embedded/Sidecar/Web, `SubtitleFormat`, `SubtitleLanguage` BCP-47, title, `SubtitleTrackDisposition` default/forced/auto).
- Реплика: `SubtitleEvent` (start < end, half-open `[start,end)`, `SubtitleLayer`, `SubtitleContent` — `#[non_exhaustive]`, пока только `Text(SubtitleTextCue{StyledText, CuePlacement})`; ASS (08) и bitmap/PGS (11) добавляются вариантами, заглушек нет).
- `StyledText` = спаны `TextSpan` (SpanStyle: weight/slant/underline/strike/color/background — `None` = «стиль пользователя»; voice, lang, ruby_annotation, reveal_at для караоке). Авторские цвета сохраняются поверх стиля пользователя (решение владельца).
- `SubtitleTimeline`: события в порядке вставки (`SubtitleEventId` = номер вставки, стабилен), сортировка по (start,id) + дерево максимумов end (`timeline/max_end_tree.rs`). `collect_active_at(t, &mut Vec<id>)` — O((k+1)log n), без аллокаций при достаточной ёмкости, порядок (layer, start, id). `next_change_after(t)` — для «растеризуем только при изменении». `insert` O(n) (поток демукса), bulk — `from_events`/`extend`. Дубликаты (start,end,layer,content) игнорируются — повторное чтение пакетов после seek.

## Разбор (subtitle-formats)
- Вход: `load_text_subtitle(bytes, TextSubtitleFormat::{Srt,WebVtt}, LegacyEncodingHint::{Cyrillic,Generic}, &SubtitleParseLimits)`. Лимиты (байты, события, число записей в отчёте) приходят от вызывающего, без Default. Фатально: `InputTooLarge` (до декодирования), `TooManyEvents`, `NotWebVtt`. Проблемы реплик → `SubtitleParseReport` (skipped/empty/ignored_style_rules/unreported_issues + `ParseIssue` с номером строки).
- Кодировки (решение владельца): BOM → UTF-16 без BOM (NUL-шаблон, до проверки UTF-8 сознательно) → UTF-8 → chardetng (tld `ru` для Cyrillic; tld только из внутреннего enum — chardetng паникует на плохом tld). `DetectedSubtitleEncoding::encoding_name()` — для уведомления «read as …» (тексты UI — английские).
- `sniff_text_subtitle_format` — WEBVTT раньше SRT.
- Известные отличия: SRT `,5` = 0,5 с (ffmpeg/VLC — 5 мс); WebVTT строгий W3C (пустая строка-разделитель, STYLE/REGION до первой реплики); подмножество CSS — см. матрицу.

## Сессия 11 (PGS)
`SubtitleContent::Picture` (`picture.rs`); шкала умеет `shorten_event_end` + `revision()` + журнал укорочений — `mem:subtitles/pgs-s11`.

## Тесты
- `crates/subtitle-core/src/timeline/tests.rs` (в т.ч. сверка с перебором на псевдослучайных данных), `crates/subtitle-formats/tests/{srt_loading,encoding_and_limits,webvtt_loading}.rs` — сквозь `load_text_subtitle` → `SubtitleTimeline::collect_active_at`.
