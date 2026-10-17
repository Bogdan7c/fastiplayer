# ASS слой 2 (сессия 09, этап 2): повороты, наклон, 3D, `\org`, `\move`, `\fad`/`\fade`

Поведение — порт libass 0.17.5 (ISC-шапки в перенесённых файлах). Поддержано: `\frx \fry \frz \fr \org \fax \fay` (+ вместе с `\fscx \fscy \fsp`), `\move` (4 и 6 аргументов), `\fad \fade` (2 и 7 аргументов), 3D-проекция libass. НЕ поддержано (план 10): `\t \clip \iclip \blur \be \k* \p`, эффекты Banner/Scroll — остаются `AssUnsupportedTag`.

## Новые модули
- subtitle-core: `ass/geometry_tags.rs` (варианты `AssOverride` для поворота по оси, наклона, `\org`, затухания в сыром виде; переписывание `\fad` в t1..t4 делается при рендере, нужен Duration).
- subtitle-formats: разбор по правилам libass + отчёт-счётчики; тесты `ass/geometry_tag_tests.rs`.
- subtitle-raster:
  - `glyph_transform/rotation.rs` (порт `calc_transform_matrix`, 3D-трансформ контуров, параметры поворота: центр = `\org` или базовая точка выравнивания, тень входит в shift);
  - `compose/rotation.rs` (применение матрицы к картинкам/плашке BorderStyle 3; `m[2][2] <= 0` -> глиф отброшен без паники);
  - `layout/baseline_shear.rs` (`apply_baseline_shear` для `\fay`, опора наклона на базовой линии);
  - `ass/animation.rs` (+ `ass/animation/`): часы события (elapsed/duration), интерполяция `\move`, `interpolate_alpha`, классификация зависимости от времени;
  - `event_fade.rs` (`mult_alpha` ко всем 4 цветам и плашке; полностью прозрачные картинки выбрасываются);
  - `overlay/animation_change.rs` — нейтральный тип.
  - Эталоны: `libass_parity/ass_tolerances_layer2.rs` (статичные повороты) и `ass_tolerances_animation.rs` (время). Схема нескольких моментов в одном кейсе: файл `<case>@t<5-значные мс>` (напр. `@t01000`); генератор `scripts/subtitle-reference/generate.py` принимает несколько моментов. Допуски как в 08 (геометрия точно, <=1 % площади), отклонения обоснованы в таблицах.

## Нейтральный API
- `RenderRequest.moment: SubtitleTime` (момент показа в шкале событий), `AssEventTiming::new(start, end)`, `RenderableCue::ass(key, layer, cue, timing)`. Текст/SRT время не используют.
- `AnimationChange { None, EveryFrame, At(SubtitleTime) }`: `earliest(a,b)`, `holds_at(rendered_at, moment)`; `SubtitleOverlay::animation_change()` — минимум по событиям кадра. Значение верно для моментов не раньше момента запроса.
- Ключ кэша реплики включает всё, что зависит от времени (позиция 1/8 px, альфа затухания u8); статичное событие — ключ без времени (не растеризуется повторно).

## Размещение и коллизии
События с `\pos`, `\move`, `\org`, поворотом/наклоном -> группа `Fixed` (надпись: не поднимается над панелью, не участвует в коллизиях). Решение исполнителя, озвучено владельцу.

## Сужение срока годности в потоке (app-egui)
`render_worker/validity.rs::ValidityInterval::narrowed_by_animation(change, frame_step)`: `[t, min(next_change_after(t), анимация))`; `At(x)` -> x; `EveryFrame` -> ЦЕНТРИРОВАННЫЙ отрезок `[g - шаг/2, g + (шаг - шаг/2))` (кадр берёт картинку ближайшего времени; реальные pts 23.976 к/с не лежат на сетке); нижняя граница зажата `SubtitleTimeline::latest_change_within(g - шаг/2, g)` (картинка не показывается до начала реплики); у `ValidityInterval` теперь `valid_from <= rendered_at`, `find_exact` берёт картинку с ближайшим `rendered_at` (шаг из `PresentationClock.cadence`, едет в `RenderJob.frame_step` -> `StepInput.frame_step`); шаг неизвестен -> 1 мкс (перерисуется любой другой кадр); `None` без изменений; интервал не короче 1 мкс. Применяется в `render_step.rs` после рисования по `rendered.animation_change()`. Планировщик/запись/очередь не менялись: кадры горизонта анимированной сцены просто получают каждый свой интервал (ёмкость max(prepared_overlays, prerender+1) хватает; `repeated_jobs_suppressed == 0`). Пауза: после готовой картинки работы нет.

## Тесты
- app-egui `state/subtitles/tests/`: `ass_animation_support.rs` (сценарии/владелец), `ass_animation_gpu.rs` (GPU readback: `\move` между концами, `\fad` слабее в начале; пропуск без adapter), `ass_animation_thread.rs` (поток: свой кадр у каждого кадра + статичное один раз, пауза без работы, 50 анимированных событий: выбор в UI не блокируется; `#[ignore] heavy_scene_release_measurement` — `cargo test -p app-egui --release --locked -- --ignored heavy_scene_release_measurement --nocapture`).
- Замер (release, 50 событий `\move`+`\fad`, часть `\frz`): ~3.5 мс на кадр в потоке (вкл. пересылку), самая долгая растеризация ~6 мс, выбор в UI ~0.03 мс.
- Юнит: `validity.rs` (сужение).

## Ограничения
- Сужение `EveryFrame` использует наблюдаемый шаг: после рывка/пропуска кадров шаг вырастает, и картинка может быть показана кадру, отстоящему до полушага.
- В тестах кадры нужно подавать подряд с реальным шагом (скачки времени без seek дают неточную картинку из длинного интервала).
- Регрессия 23.976 к/с с pts в мс: `ass_animation_thread::frames_with_millisecond_rounded_pts_each_show_the_picture_nearest_their_own_time` и `animated_cue_starting_between_frames_never_shows_before_its_start`.
- `\t`, `\clip`, `\blur`, караоке, `\p`, Banner/Scroll — план 10.
