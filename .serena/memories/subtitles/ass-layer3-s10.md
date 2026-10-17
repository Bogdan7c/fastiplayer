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
- Модель: `subtitle-core/src/ass/filter_tags.rs` (`AssClip{mode Inside|Outside, shape Rect|Vector{scale, drawing}}`), `AssOverride::{Blur(Option<f64>), EdgeBlur(Option<f64>), Clip, DrawingBaselineOffset(f64)}` — сырые значения (зажатие/округление в raster, т.к. `\t` интерполирует до него). (`AssUnsupportedTag` удалён в 10b — неподдержанных тегов нет.)
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

## Караоке (10b, пакет Q3)
- Модуль `subtitle-raster/src/karaoke/`: `mark.rs` (`KaraokeMark` — захват тегов на символ, `splits_run`), `plan.rs` (`KaraokePlan::build` = `ass_process_karaoke_effects` без `tm_current`; `KaraokeWord::progress_at`), `leftmost.rs` (`LeftmostTracker` = `ass_outline_update_min_transformed_x`), `rule.rs` (`KaraokeRunRule::paint_at/next_change`), `stage.rs` (`KaraokeStage`, `ImageRole`, builder).
- Поток: `RenderState.karaoke` (pending) -> `SourceChar.karaoke` -> `TextCluster.karaoke/rotation_z_degrees` -> `mark_style_runs` (правила `split_style_runs`) -> `arrange_text` строит `TextLayout.karaoke` ПОСЛЕ measure/trim и ДО reorder (читает предварительные pos.x) -> `compose` (leftmost по `scaled` матрице, `FilterFlags` FILL_IN_SHADOW для KF/KO, правило на участок, заливка караоке не отбрасывается по alpha 0 — `images::canvas_image`) -> `RenderedText.karaoke` -> `RasterizedCue.karaoke`.
- Покадровая стадия: `RasterizedCue::final_bitmaps(clock)` -> `CueClip::apply_with_rect(rect, bitmaps, paints)`; `clip/paint.rs ImagePaint{AsStored,Hidden,Solid,SplitAt}` — разрез по `brk` внутри `push_visible_parts` (порядок как `render_glyph`/`render_glyph_i`), прозрачные куски отбрасываются там же. Срок годности: `RasterizedCue::karaoke_change` -> `FrameTimeDependence::include_stage_change` (EveryFrame во время `\kf`, `At(start)` до слога). В ключ кэша время не входит.
- Тесты: кейсы `ass_k_*` + `ass_tolerances_karaoke.rs`; `karaoke/tests.rs`; `renderer/tests/ass_karaoke.rs`; app-egui `ass_karaoke_gpu.rs`, `ass_karaoke_thread.rs`. Скорость: w_kf 0.17 мс/кадр против 0.43 у libass C.

## `\t` (10b, Q1-Q2)
- Модель: `subtitle-core/src/ass/animation_tags.rs` (`AssTransform{timing: AssTransformTiming::{Default, Accel, Range, RangeAccel}, tags}`; невалидный `\t` = пустые tags, но выключает коллизии), `karaoke_tags.rs`, `effect.rs` (`AssEffect`, `AssCue::transition_effect()`). `AssUnsupportedTag` удалён целиком. Разбор: `subtitle-formats/src/ass/transform_tags.rs` (вложенность ≤ 16 `MAX_TRANSFORM_NESTING`, тратит `TagBudget`). Парсер хранит СЫРЫЕ bord/shad/fscx/fscy (зажатие после интерполяции в raster).
- raster: `ass/tag_weight.rs` (`TagWeight`, `calc_anim`, `change_color/alpha`, `TransformSpan` k-формула), `transform_apply.rs` (`TagSink`; вложенный `\t` заменяет вес), `transform_scan.rs` (`digest_transforms`: At(t1+1) если вес в t1 = 0, иначе At(t1); EveryFrame в [t1,t2); веса в ключе только для `\t` с растровыми тегами; `animates_orientation` -> группа Fixed), `clip_animation.rs` (`\t` только с прямоугольным клипом не входит в ключ — клип считается покадрово в `final_bitmaps(clock)`; memo `OnceLock` только для время-независимого).
- Кейсы `ass_t_*`, допуски `ass_tolerances_transform.rs`; тесты `renderer/tests/ass_transform.rs`, app-egui `ass_transform_thread.rs`.

## Banner/Scroll (10b, Q4)
- `ass/scroll.rs` (`ScrollClock`: delay -> (int)max(delay/scale,1)*scale, shift дробный -> EveryFrame всё время показа), `event_style::ScrollPlacement` в `ExactPlacement.scroll`, `reorder::LineAlignment::{InTextArea, InWidestLine}` (HSCROLL), `EventClipPlan::limit_to_scroll_band`, `AssAnimationKey::Sampled.scroll_shift_bits`, `evaluate_animation(.., &AssFrameScale)`. Эффекты — группа Fixed (как `\move`). Кейсы `ass_fx_*`, `ass_tolerances_effect.rs`, `renderer/tests/ass_effect.rs`, app-egui `ass_effect_thread.rs`. Аудит всех тегов libass: `subtitle-formats/src/ass/libass_tag_list_tests.rs`.

## Нулевой размер и караоке (после ревью 10b)
Буква или `\N` в стиле нулевого размера — «фантом» (`LookPolicy::make_phantom_look`, `event_plan::letter_look_index`): ширина 0, без контура/тени, 1 px для шейпинга; `take_karaoke_mark` вызывается для КАЖДОЙ буквы и разрыва (как GlyphInfo в libass `parse_events`), иначе метка караоке «переезжает». `\N` нулевого размера — настоящий разрыв. Альфа в модели — сырое `Option<i32>`, маска младшего байта после интерполяции (`tag_weight::blend_alpha`). Тест: `renderer/tests/ass_karaoke_zero_size.rs`.

## Ограничения
- Маска векторного клипа растеризуется заново при каждом новом ключе анимации реплики (`\move`/`\fad`) — в 10c.
- Целые сцены медленнее libass (кэш реплики целиком; `\move`, `\t` с растровыми тегами, Banner/Scroll = полная перерисовка каждый кадр, Banner — даже за краем экрана) — 10c.
- NaN в `\blur` -> 0, NaN размер шрифта -> размер стиля (libass оставил бы NaN).
- Плавающий тест app-egui `language::new_media_with_another_track_language_redraws_with_the_new_language` (~1 из 6 прогонов), к слою 3 не относится — сессия 14.
