# ASS слой 3 (сессия 10): рисунки, клипы, размытие (10a); `\t`, караоке, Banner/Scroll (10b); кэш как libass (10c)

Порт libass 0.17.5 (commit 4a05d812, ISC-шапки). План/отчёт: `user/subtitles/10-ass-layer-3.md`, `user/subtitles/results/10.md`.

## Решения владельца (2026-10-17)
- Три этапа в одной сессии, каждый — свой коммит и приёмка: 10a `\p \pbo \clip \iclip \blur \be`; 10b `\t`, `\k \K \kf \ko \kt`, Effect Banner/Scroll, итоговая матрица; 10c трёхуровневый кэш (контуры / маски глифов / склейка) как libass.
- Клипы и караоке накладываются ПОСЛЕ кэша реплики (не в ключе кэша), как в libass.
- Бюджет скорости: новый код не медленнее C-версии libass (без SIMD) на тех же картинках; для целых сцен (тяжёлое караоке ≤ 2.5 мс/кадр 1080p) — в 10c.
- Рисунки при `Fontsize: 0` — как libass (рисуются); буквы нулевого размера у libass ~16 px (особенность FreeType) — у нас не рисуются, известное отличие (кандидат в сессию 14).
- «Мой стиль» (`AssStyleSource::User`): авторские рисунки скрыты, авторское размытие игнорируется.
- Буферы размытия: config v20 `[subtitles] blur_scratch_retained_mb` (32, 1..=1024) -> `RasterCacheLimits.blur_scratch_retained_bytes`.

## Карта (10a)
- Модель: `subtitle-core/src/ass/filter_tags.rs` (`AssClip{mode Inside|Outside, shape Rect|Vector{scale, drawing}}`), `AssOverride::{Blur(Option<f64>), EdgeBlur(Option<f64>), Clip, DrawingBaselineOffset(f64)}` — сырые значения (зажатие/округление в raster, т.к. `\t` интерполирует до него). `AssUnsupportedTag` = только `Transform`, `Karaoke`.
- Разбор: `subtitle-formats/src/ass/filter_tags.rs`; в режиме `\p` текст до следующего `{` — один сырой `AssTextItem::Text` (без `\N \h`), `text.rs`.
- raster:
  - `ass/filter_params.rs` (`FilterParams` в `RenderState.filters`: rect clip last-wins, vector first-wins, `\r` сбрасывает blur/be);
  - `drawing/` (tokenizer, `ass_strtod`, кривые в i32 26.6) -> псевдоглиф U+FFFC (`SourceChar.drawing`, `SourceText.drawings`, `TextCluster.drawing`, `PlacedGlyph.outline_offset_y`, `glyph_render/drawing.rs` метрики `get_outline_glyph`, `ass/drawing_scale.rs`);
  - `blur/` (побитовый порт C-ядер: gaussian каскад + box `\be`, `BlurScratch` у `SubtitleRasterizer`, quantize/restore, паддинг) -> `compose/filter.rs` `RunFilter`, `compose/run.rs composite_run` (порядок `ass_composite_construct`), `SpanLook::blur`;
  - `clip/` (rect: `render_glyph`/`render_glyph_i` 4 куска; vector: `blend_vector_clip`; маска строится только в пределах холста через `Rasterizer::fill_tiles`/`outline_to_bitmap_within` — побитово как полная); `ass/clip_plan.rs` (PlayRes -> экран, `x_to_screen_ignoring_pixel_aspect`); `RasterizedCue{clip, clipped_bitmaps: OnceLock}` + `final_bitmaps()` — стадия после кэша, клип до сдвига коллизий; `overlay/crop.rs` сохраняет владельца картинки. Подложка `Background` не клипуется (libass `add_background`).
  - Камера 3D: `20000 * blur_scale_y` через `SpanLook::camera_scale` (была ошибка 09: всегда 20000).
  - `double_to_d6` = C `lrint` -> int (wrap mod 2^32, NaN/inf -> 0).
- Статистика: `invalid_ass_drawings` (бывш. `skipped_ass_drawing_runs`), `failed_blur_layers`; `skipped_oversized_bitmaps` — неразмытые из-за размера слои.

## Тесты и эталоны
- Кейсы `ass_draw_*`, `ass_blur_*`, `ass_clip_*`, `ass_rot_camera_storage` в `crates/subtitle-raster/tests/fixtures/libass/cases`, допуски `src/libass_parity/ass_tolerances_layer3.rs`. Генератор при каждом прогоне переписывает `expected/text_box4.json` (неинициализированный `type` у libass) — откатывать `git checkout`. Сравнение игнорирует картинки libass нулевого размера (векторный клип).
- Ядра размытия: `blur/tests/` + золотые `tests/fixtures/blur/`; сверка с живым C — `#[ignore] blur::c_parity` (нужен харнесс на libass без asm, `LIBASS_BLUR_HARNESS`).
- Публичный API: `renderer/tests/{ass_drawing,ass_blur,ass_clip}.rs`; GPU в кадре: app-egui `state/subtitles/tests/ass_layer3_gpu.rs`.
- Замер сцен: `cargo run -p subtitle-raster --release --example measure_ass_scene -- <file.ass> <start_ms> <frames>` (1920x1080, шаг 1000/24, только встроенные шрифты — как бенч libass).

## Ограничения (10a)
- Маска векторного клипа растеризуется заново при каждом новом ключе анимации реплики (`\move`/`\fad`) — в 10c.
- Целые сцены медленнее libass (кэш реплики целиком; `\move` = полная перерисовка) — 10c.
- `\t`, караоке, Banner/Scroll — 10b.
