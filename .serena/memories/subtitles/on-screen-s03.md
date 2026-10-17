# Субтитры на экране (сессия 03, 2026-10-15)

План/отчёт: `user/subtitles/03-gpu-pass-and-sidecar.md`, `user/subtitles/results/03.md`. Модель/растеризатор — `mem:subtitles/core`, `mem:subtitles/raster`.

## Цепочка
файл рядом (`subtitle-sidecar`) → фоновая задача в app-egui (поиск → чтение с лимитом → `load_text_subtitle` → `SubtitleTimeline`, первая задача строит `SubtitleFontLibrary` с системными шрифтами) → по кадру: pts отправляемого кадра → активные реплики → `SubtitleRasterizer::render` только при изменении → `SubtitleOverlayFrame` в `RenderFrameInput` → GPU-проход в `render-wgpu-shell` между видео и egui.

## Владельцы и границы
- `crates/subtitle-sidecar` (нейтральный, deps subtitle-core + thiserror, allowlist `SUBTITLE_SIDECAR_ALLOWED_DEPENDENCIES`): `find_sidecar_subtitles(video_path, &SidecarSearchLimits, is_cancelled)`. Правила (решение 13): папка видео + `Subs|Sub|Subtitles|Subtitle` (ASCII any case) + `Subs/<stem>/` (там любое имя); имя = stem видео + конец/разделитель `. _ - space [ (`; языки из суффикса (коды через `SubtitleLanguage`, таблица англ. названий), `forced`/`sdh|cc|hi`; ass/ssa находятся, `is_renderable_now()=false`. Порядок: точное имя → папка видео → Subs → Subs/<stem>, внутри — байтовый порядок имени. Пути без потерь.
- `render_core::{PictureSize, PictureRect, PictureLayout}` (`picture_rect.rs`) — единственная формула «картинка без полос»; `letterbox_scale_and_offset` в render-wgpu-video делегирует ей (uv бит-в-бит, тест на сетке).
- `render-wgpu-shell/src/subtitle_overlay/` — `SubtitleOverlayRenderer` (pub, переиспользуемая wgpu-реализация): R8 атлас (shelf packer, растёт ×2 до max_texture_dimension), `textureLoad` без sampler, premultiplied blend `vec4(rgb*a,a)` (dst alpha видео = 1 → верно для обоих alpha-режимов surface; блендинг в гамма-пространстве), scissor = зажатый video viewport. Загрузка только при смене содержимого: revision + `Arc::ptr_eq` масок (revision один не годится — новый растеризатор начинает те же номера). Смещение картинки → только 16-байтный uniform. `SubtitleOverlayUploadStats`, `Renderer::subtitle_overlay_upload_stats()`.
- **Контракт для второго рендера (GLES2, #6)**: `SubtitleOverlay` + `PictureRect` + `SubtitleLayerOffset` (с сессии 05, `mem:subtitles/appearance-edit-s05`) + размер экрана; canvas (0,0) → округлённый (левый верх picture rect + offset); 1 px холста = 1 px surface (растеризуем с canvas = округлённый размер картинки).
- app-egui: `state/subtitles.rs` (`SubtitleOwner`, жизненный цикл Idle/Loading/Ready/NoSubtitleFile/Unavailable) + `state/subtitles/{limits,active_track,job_runner,sidecar_job,presentation}.rs`. Точка запуска — `AppState::record_installed_media_observables` (`state/media_jobs.rs`), общая для Open/DnD/playlist next/пересборки/resume; сброс — `clear_released_installed_media_source`, `record_cleared_media_after_exact_reset`. Wake — `AppWakeOwner::Subtitles` → `AppState::poll_subtitle_jobs`. Подключение к кадру — `frame_prepare/submit.rs` (`SubmittedFrameForSubtitles::from_submission` + `AppState::subtitle_overlay_for_submitted_frame`); `frame_prepare.rs` не трогали.
- Язык для выбора варианта CJK: `TextLanguagePreference::track_then_system` (CJK-язык дорожки из sidecar-суффикса, иначе язык системы из `LC_ALL`/`LC_MESSAGES`/`LANG`, читается один раз при старте — `state/subtitles/system_language.rs`) → `RenderRequest.text_language`. Ранжирование лиц — `mem:subtitles/raster`.
- Лимиты — из конфига `[subtitles]` (v13, сессия 04); в `state/subtitles/limits.rs` только внутренние инварианты; бюджет растеризации 8 мс (warn).

## Инварианты
- Показ только если MediaInstanceId владельца == snapshot И `render_generation` кадра == snapshot; кадр без identity (stale-lease при seek) — без субтитров.
- Время = pts отправляемого кадра через `SubtitleSyncOffset` (Z/X ±0.1 с с сессии 04). Пауза → тот же кадр → тот же overlay без растеризации/загрузки.
- **Известное ограничение:** pts кадра — сырой timestamp контейнера (нормализации старта нет нигде); у MPEG-TS/M2TS с ненулевым стартом sidecar разъедется. Поправку вносить в одной точке (`SubtitleSyncOffset`/lookup) — сессия 14 или отдельное решение владельца.
- Выбор дорожки (с сессии 04): автовыбор по языкам, меню CC, хоткеи — `mem:subtitles/controls-s04`. Задача поиска хранит ВСЕ кандидаты, читает только выбранную.

## Замеры (release, 2026-10-15)
Новая реплика 0.1–0.8 мс до 4K; холодный старт CJK ~3.5 мс (загрузка системного лица); библиотека шрифтов ~94 мс один раз в фоне. CPU вживую 720p: 11.2 % без субтитров vs 11.3 % с ними.

## Тесты
- `render-core/src/tests/picture_rect.rs`; `subtitle-sidecar/tests/sidecar_search.rs`; `render-wgpu-shell/src/subtitle_overlay/tests.rs` (GPU readback, пропуск без адаптера); `app-egui/src/state/subtitles/tests/{gpu_chain,presentation,sidecar_job}.rs`.
- Real-media `#[ignore]`: `FASTIPLAYER_SUBTITLE_ASSETS_DIR=<abs>/test-assets/subs cargo test -p app-egui --locked -- --ignored real_sidecar` (сценарии 01–11, описание — `test-assets/subs/README.md`, owner-local, git-ignored; Sintel CC-BY). Замер: `cargo run -p subtitle-raster --release --locked --example measure_render_time`.


## Сессия 08
Сдвиг слоя стал двухгрупповым: `SubtitleLayerOffsets{main, fixed}` (SRT — `uniform`), картинки помечены `OverlayPlacementGroup`. Размещение: `LayerPlacementPolicy::{UserPlaced{bounds: PictureAndBars|PictureOnly}, AuthorPlaced, LiftMainGroupOnly}`; ASS никогда не на полосах — `mem:subtitles/ass-layer1-s08`.