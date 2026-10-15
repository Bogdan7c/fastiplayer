# Субтитры: subtitle-raster (сессия 02, 2026-10-15)

План/отчёт: `user/subtitles/02-raster-foundation.md`, `user/subtitles/results/02.md`. Модель — `mem:subtitles/core`.

## Граница
- `crates/subtitle-raster` — нейтральный растеризатор, deps только subtitle-core, skrifa 0.44, harfrust 0.12 (одна read-fonts 0.41 с egui — не обновлять порознь), fontdb, unicode-bidi, thiserror, tracing. Guardrails: `SUBTITLE_RASTER_ALLOWED_DEPENDENCIES` + `test_subtitle_raster_rejects_gpu_dependency`.
- Порт libass 0.17.5 (тег, commit 4a05d812), ISC-шапки в перенесённых файлах. Код FreeType/HarfBuzz/FFmpeg/VSFilter не копируется.

## Публичный API
- `SubtitleRasterizer::new(SubtitleFontLibrary, RasterLimits{cache: RasterCacheLimits{cue_bitmaps_bytes, shaper_faces}, input: RasterInputLimits{max_chars_per_cue, max_cues_per_frame, max_canvas_side}})` — без Default (дефолты придут из конфига в сессии 04).
- `render(&RenderRequest{cues: &[RenderableCue{key: CueKey(CueSourceId, SubtitleEventId), cue: &SubtitleTextCue}], canvas: CanvasSize, appearance: &SubtitleAppearance}) -> &SubtitleOverlay` — без Result: ошибки пореплечные (пропуск + счётчик `failed_cues`, warn без текста).
- Контракт `CueKey`: один ключ = одно содержимое; сменилось содержимое дорожки → вызывающий меняет `CueSourceId`.
- Fast path: те же ключи/холст/стиль → прошлый overlay, `OverlayChange::Unchanged`, без аллокаций. `Moved` пока недостижим через API (нужен с анимациями).
- Выход: `SubtitleOverlay{canvas, bitmaps: Vec<SubtitleBitmap{origin, w, h, stride, coverage: Arc<[u8]>, color (прямая альфа, в отличие от libass), kind Background|Shadow|Outline|Glyph}>, revision, change}`. Инвариант: каждая картинка целиком внутри холста. `composite_rgba_over_transparent()` — эталонный композит (тесты, приёмка).
- Шрифты меняются только через растеризатор (`register_attached_font`, `load_system_fonts`) — он сбрасывает кэши. `load_system_fonts` строит индекс покрытия (~90 мс / ~1000 лиц) — вызывать в фоне.
- Стиль: `SubtitleAppearance::youtube_like()` / `outline_and_shadow()`; «em» = высота ячейки libass (FontSizeFraction × высота холста).

## Шрифты
- 22 встроенных Noto unhinted (`crates/subtitle-raster/fonts/`, README + OFL.txt, notofonts.github.io 5c391306): Sans Regular/Bold LGC + Regular алфавитов (арабский, иврит, индийские, тайский, лаосский, грузинский, армянский, эфиопский, кхмерский, бирманский, сингальский, символы). CJK/эмодзи — только система.
- Запрос по имени: attached → system → embedded; fallback по глифу: основное → встроенные → вложенные → системные; нигде нет → .notdef + `missing_glyph_lookups`. Системные кандидаты ранжируются (`fonts/system_fallback.rs`): наклон основного лица, ближайший вес OS/2, затем семейство/путь/индекс; в `shaping::build_clusters` уже использованные в реплике системные лица проверяются первыми (как `font->faces` в libass). Язык-осведомлённый CJK fallback: `RenderRequest.text_language: TextLanguagePreference` (`unknown()`, `preferred(&SubtitleLanguage)`, `track_then_system(track, system)`: известный язык дорожки решает всегда, иначе язык системы; не-CJK = нет предпочтения). Критерий «совпадение языка» стоит в ранге ПОСЛЕ наклона и ДО веса: Exact (письменность+регион, zh-HK↔HK, zh-TW/zh-Hant↔TC) > Script (ja↔Jpan, zh/zh-Hans↔Hans, ko↔Kore) > лицо без языка > лицо другого CJK-языка. Языки лица считаются при сканировании (`fonts/face_language.rs`): таблица `meta`/`dlng` (skrifa `font.meta()`), иначе эвристика по токенам имени семейства (jp/kr/sc/cn/tc/tw/hk); у Noto Sans CJK `meta` нет, работает эвристика. Язык входит в ключ lookup-кэша глифов, кэша реплик (`CueCacheKey`) и подпись кадра. Ограничения: языка отдельных спанов нет в `TextSpan` (follow-up, ASS `\fe`/lang); `zh-Hant` без региона = Тайвань; pan-CJK лицо с несколькими языками в `dlng` равноценно специализированным. app-egui: язык системы читается один раз в `state.rs` (`subtitles::read_system_language`, LC_ALL/LC_MESSAGES/LANG), язык дорожки — из `SidecarSubtitleCandidate.language` -> `ActiveSubtitleTrack::language()`; смена дорожки меняет `source_id`, поэтому reuse overlay безопасен. Непригодное лицо помечается и больше не выбирается. Байты системных файлов — Weak в библиотеке, сильные у FaceCache.

## Паритет с libass (важные находки)
- Растеризатор/stroker/shift/fix_outline — побитно (0 px) на рисунках и 50k случайных контурах.
- Ширины букв libass берёт у FreeType после автохинтинга (FT_LOAD_DEFAULT), контуры — без хинтинга: воспроизведено через хинтер skrifa; без этого строки уплывают до 1 px.
- Текст 720p: 3–120 отличающихся px (wrap 419, арабский ~1000 — 1 px сдвиг у 2 глифов). Допуски в `src/libass_parity/text_tests.rs` (`Tolerance`).
- libass с провайдером NONE не делает fallback по глифу — в арабском эталоне шрифт задан явно.
- Encoding=1 в libass = базовое направление LTR; `BaseDirection::Auto` только для -1.

## Эталоны и тесты
- Генератор: `scripts/subtitle-reference/` (системный libass, C компилируется во временный каталог; в сборке/CI нет C). Кейсы/эталоны: `crates/subtitle-raster/tests/fixtures/libass/{cases,expected}`. Дамп «наш/libass/diff»: `SUBTITLE_PARITY_DUMP_DIR=<dir> cargo test -p subtitle-raster --lib text_events`.
- Функциональные тесты публичного API: `src/renderer/tests/{geometry,caching,robustness,limits,font_fallback}.rs`. Ручная приёмка: `cargo run --release -p subtitle-raster --example render_samples -- <dir>`.

## Не сделано
Подчёркивание/зачёркивание, фон спана, синтетический bold/italic, вертикальный текст, регионы VTT, переносы CJK без пробелов, blur/теги ASS (08–10).
