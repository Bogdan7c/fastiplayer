# Субтитры, сессия 08 (2026-10-17): ASS/SSA слой 1 — порт libass

План/отчёт: `user/subtitles/08-ass-layer-1.md`, `user/subtitles/results/08.md`. Растеризатор — `mem:subtitles/raster`; встроенные дорожки — `mem:subtitles/embedded-tracks-s06`; GPU-проход/размещение — `mem:subtitles/on-screen-s03`.

## Источник
libass 0.17.5 (тег, commit 4a05d812, ISC). ISC-шапки в перенесённых файлах. Не копировать FFmpeg/VSFilter/FreeType/HarfBuzz; синтетический жирный — свой алгоритм по описанию (`outline/synthetic.rs`).

## Карта libass → Rust
- `ass.c` (секции, стили V4+/V4, события, chunk MKV, PlayRes по `ass_lazy_track_init`) → `subtitle-formats/src/ass/` (`script_builder`, `style_line`, `event_fields`, `chunk`, `text`, `tags`, `hard_overrides`); модель → `subtitle-core/src/ass/` (`AssScript` с индексом стилей по имени, `AssStyle`, `AssCue{script: Arc, style, read_order, margins, hard_overrides, text}`, `AssOverride` вкл. `Move`, `Drawing(AssDrawingMode)`, `Unsupported`). `SubtitleContent::Ass(AssCue)`.
- `ass_parse.c` теги → `tags.rs` (порядок `else if` libass, префиксное сопоставление); применение → `subtitle-raster/src/ass/render_state.rs` (init/reset render context, first-wins `\an`/`\pos`/`\move`, `\r`).
- `ass_render.c`: масштаб `ass/frame_scale.rs`; план события `ass/event_plan.rs`; selective font scale и «мой стиль» `ass/look.rs`; явность `ass/explicit.rs` (флаг из парсера по сырому тексту); `get_base_point`/device pos → `layout/`; `fix_collisions` с памятью → `renderer/ass_collisions.rs`; слои → `renderer/ass_frame.rs`; scale/spacing/kerning → `shaping` + `glyph_render/retrieve`; synth bold/italic + подчёркивание → `glyph_render/{face_style,decoration,styled}.rs`.

## Публичные границы
- raster: `RenderableCue::{text, ass(key, layer, cue)}`, `CueContent`, `RenderRequest.ass: AssRenderOptions{text_scale, style_source Author|User, storage_size}` (в ключе кэша `CueLook`, подписи кадра и `ShownOverlay::is_same_as`); `RasterInputLimits.max_ass_events_per_frame` (ASS сортируются (layer, read_order) до отсечения); `SubtitleBitmap::placement_group() -> OverlayPlacementGroup{Main, Fixed}` (Main = без `\pos/\move` и выравнивание по низу; SRT — Main), `SubtitleOverlay::group_bounds`.
- shell: `SubtitleLayerOffsets{main, fixed}` (uniform 32 B, instance 32 B с group_index; смена сдвига не перезагружает текстуры).
- player-core: `SubtitleFeedMediaStart.track_headers: Vec<SubtitleTrackHeader{track_id, codec_private}>`.
- app-egui: `owner_embedded/{ass_tracks,ass_owner}.rs` (ленивый разбор заголовка, ReadOrder-set на дорожку, сброс при пересборке дорожки; без длительности → событие невидимо), sidecar `.ass/.ssa`, `presentation/ass_options.rs`, `owner_placement/ass_positioning.rs`, `layer_placement/group_lift.rs`.
- Конфиг v18: `ass_position_mode` = `main_lines_avoid_controls` (default: основные реплики над панелью, надписи на месте, сдвиг пользователя не применяется) | `author` | `whole_layer` (в пределах картинки); `max_ass_header_kb` 1024, `max_ass_styles` 1000, `max_ass_override_tags_per_event` 512, `max_ass_events_on_screen` 256. ASS никогда не на полосах.

## Решения владельца
Scale/Spacing в слое 1; «мой стиль» на все реплики (позиции авторские); дефолт сдвига выбран после живой приёмки (в окне панель видна всегда).

## Эталоны и тесты
Кейсы `ass_*` в `crates/subtitle-raster/tests/fixtures/libass/cases` (генератор поддерживает `storage_width/height` в заголовке); таблица допусков `src/libass_parity/ass_tolerances.rs` (≤1 % площади, геометрия точно). GPU-цепочка: app-egui `state/subtitles/tests/{ass_layer1_gpu,ass_position_gpu,ass_position_lift_gpu,embedded_ass,ass_sidecar}.rs`, фикстура `crates/symphonia-demux/tests/fixtures/subtitles/ass-layer1.mkv`. Приёмка вживую: libass-эталон кадра — `ffmpeg -vf subtitles=<mkv>` (у mpv `--vo=image` субтитры не рисуются); фуллскрин — KWin-скрипт через `qdbus6 org.kde.KWin /Scripting loadScript`.

## Ограничения
Дорожки из TracksChanged — текст без стилей; `\fn@` вертикальный не поддержан; `\move` стоит в начальной точке; теги слоёв 2–3 игнорируются (счётчик); растеризация события 8–11 мс (warn бюджета UI 8 мс).
