# Сессия 08: субтитры и диагностика на GLES2

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 08 плана `user/gles2-renderer/`: перенеси на GLES 2.0 проход субтитров (слой масок R8 и слой RGBA-картинок PGS, премультиплицированное смешивание, две группы сдвига) по готовому контракту для второго рендера и выведи диагностику кадра в тех же именах трасс, что и у wgpu, чтобы инструменты замера работали без правок. Сначала прочитай `user/gles2-renderer/README.md` и `results/05.md`.

## Контракт (уже записан для GLES2 — `mem:subtitles/on-screen-s03`, `mem:subtitles/pgs-s11`)

- Вход: `SubtitleOverlay` + `PictureRect` + `SubtitleLayerOffsets { main, fixed }` + размер экрана. Начало холста = округлённый (левый верх picture rect + сдвиг группы). 1 пиксель холста = 1 пиксель surface, без масштаба и фильтрации.
- Маска: `rgb = color.rgb * coverage * color.a`, `a = coverage * color.a`. Картинка: `texel * color.a`. Смешивание `ONE, ONE_MINUS_SRC_ALPHA`.
- Порядок — порядок списка, подряд идущие одного вида — одним draw-ом (`runs.rs`). Scissor = видимая область видео ∩ экран. Слишком большие картинки пропускаются со счётчиком.
- Загрузка только при смене содержимого (revision + `shows_same_pixels_as`). Сдвиг — только uniform.

## Перенос на GLES2 (разведка, `subtitle_overlay.wgsl`)

- Нет инстансинга → 4 вершины (или 6 с индексами) на квад, атрибуты копируются на вершины.
- Нет flat-varying-ов → одинаковые значения на всех вершинах квада, интерполяция безвредна.
- Нет `textureLoad` → `texture2D` с `NEAREST` и `(atlas_origin + floor(local) + 0.5) / atlas_size` (uniform размера атласа).
- Нет целочисленных атрибутов → float.
- Индекс группы → CPU сразу выбирает origin, или `mix` между двумя uniform-ами.
- Атлас R8 — `GL_LUMINANCE`/`GL_RED_EXT`, рост ×2 до `MAX_TEXTURE_SIZE`; RGBA-слой создаётся лениво.

Переиспользуемость: в `gles2-video` это общий «слой bitmap-оверлеев» (маски + RGBA, свой словарь), адаптер в `render-gles2-shell` переводит `SubtitleOverlay`. Так оверлей пригодится и Fasticut-у (титры).

## Диагностика (паритет для инструментов)

- `RenderFrameStageTimings` с теми же именами стадий (`video_render`, `egui_render`, `subtitle_overlay`, `window_corner_mask`, `surface_present`, …); у GLES2 нет некоторых стадий (`device_poll`) → 0, не пропуск поля. `agg_timing.py` читает `*_ms` по имени.
- `fastiplayer::video_render_acceptance` — эмитит приложение после `RenderFrameOutcome::Presented`. Проверить, что GLES2 возвращает этот исход только после реального swap.
- Строки плана/бэкенда в логе: `plan="vaapi-dmabuf-gles2"`, `plan="ffmpeg-host-upload-gles2"`, бэкенд «VA-API EGL DMA-BUF». `docs/benchmarks/tools/local/evidence.py` и `t480s/collect.py` научить классифицировать их (сейчас знают только wgpu-строки).
- `RenderDiagnostics.active_color_path`, `video_draw_rect_count`, счётчики загрузок субтитров, импорта, кольца — в той же трассе.

## Тесты

Порт набора `render-wgpu-shell/src/subtitle_overlay/tests*.rs` на GLES2 headless. Те же названия и те же ожидания по пикселям и счётчикам:
- реплика на нижнем крае letterbox-картинки;
- тот же revision → 0 загрузок, новый → 1;
- пересозданный растеризатор с тем же revision всё равно загружается;
- сдвиг картинки = только uniform;
- субтитры не рисуются вне видимой области видео;
- группы main/fixed двигаются независимо;
- PGS-картинка поверх текста по порядку списка;
- премультиплированное смешивание полупрозрачной картинки;
- обрез по краю холста пиксель-в-пиксель;
- слишком большая картинка пропущена, соседи нарисованы.

И кросс-рендерный паритет на тех же входах с wgpu.

## Критерий выхода

- Набор тестов субтитров зелёный на GLES2, паритет с wgpu подтверждён.
- `agg_timing.py`, `evidence.py`, `collect.py` корректно читают и классифицируют прогоны GLES2 (тест инструментов `test_collect.py` дополнен).

Memories: `mem:subtitles/on-screen-s03`, `mem:subtitles/pgs-s11`, `mem:subtitles/ass-layer1-s08` (группы), `mem:subtitles/render-thread-s09`, `mem:cpu-parity/hw-qualification-s06-2026-10-21` (evidence).
