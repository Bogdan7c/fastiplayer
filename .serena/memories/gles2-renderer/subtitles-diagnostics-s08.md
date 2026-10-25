# GLES2: субтитры и диагностика кадра (сессия 08 плана gles2-renderer, 2026-10-25)

Отчёт: `user/gles2-renderer/results/08.md`. Наследство для 09 — раздел «Наследство сессии 08» в `user/gles2-renderer/09-app-integration-selection.md`.

## Контракт (общий для wgpu и GLES2)
- `render_shell_api::SubtitleCanvasOrigins::for_picture(picture_rect, offsets)` / `SubtitleOverlayFrame::canvas_origins()` — ЕДИНСТВЕННАЯ формула угла холста (round суммы угла картинки и сдвига группы, `f32::round`; NaN/inf → None). wgpu `subtitle_overlay/mod.rs` делегирует ей (прежние `placement_origin(s)` удалены, тесты переехали в `render-shell-api/src/tests/subtitle_frame.rs`).

## `gles2-video::overlay` (переиспользуемый слой bitmap-оверлеев, без субтитров)
- Словарь: трейт `OverlayBitmap: Clone` (`rect`, `pixels() -> OverlayPixels{format, bytes, stride}`, `tint`, `placement_group`, `shows_same_pixels_as`), `OverlayPixelFormat::{Coverage, PremultipliedRgba}`, `OverlayPlacementGroup::{Main, Fixed}`, `OverlayTint` (прямой RGBA8).
- `BitmapOverlayRenderer<B>::{new(context, OverlayLayerOptions), prepare(&ActiveFrame, &[B]) -> OverlayPrepareReport, draw(&ActiveFrame, &OverlayDrawRequest{origins, clip}) -> OverlayDrawOutcome::{Drawn, NothingPrepared, NothingVisible}, forget_prepared, stats() -> OverlayLayerStats{coverage, picture: OverlayAtlasStats, uniform_updates, draw_calls}, max_atlas_side, destroy}`. Слой хранит клоны загруженных картинок (держат Arc буфера → сравнение по адресу безопасно).
- Порт wgpu 1:1: `overlay/atlas.rs` (shelf packer — копия, guardrail не даёт общий crate), `runs.rs` (серии по порядку списка), `layer.rs` (`AtlasLayer`: resident-сверка, рост ×2, ленивый RGBA-атлас), правила счётчиков те же.
- GLES2-перенос: 6 вершин на картинку (24 байта: pos f32×2, atlas_uv f32×2, tint u8×4 норм., group f32), координаты атласа считаются на CPU в углах и интерполируются → центр пикселя = центр текселя, `NEAREST`, без адресной арифметики во фрагменте; группа — `mix(main, fixed, a_group)`; маска `LUMINANCE`/`RED_EXT` (`.r`), картинка RGBA8; `glBlendFunc(ONE, ONE_MINUS_SRC_ALPHA)`; после прохода blend и scissor выключены (иначе следующая заливка кадра обрезалась бы scissor-ом).
- Загрузка строк: без хвостов — 1 вызов; с хвостами — `UNPACK_ROW_LENGTH` (если stride кратен байтам на тексель) или по строке (`overlay/upload_calls.rs`); без переупаковки. GL-вызовы — `gl_calls/overlay.rs` (подмодуль модуля безопасности).
- Без `highp` во фрагменте атлас ≤ `AtlasSideLimit::MEDIUM_PRECISION_MAX_SIDE` = 256 (mediump 2⁻¹⁰ против половины текселя); `AtlasSideLimit::AtMost(n)` — для тестов.
- `gles2_egui::FrameLayerStack::new(&mut underlay, &mut overlays)` — подложка одного слоя + оверлеи другого.

## `render-gles2-shell`
- `Gles2SubtitleOverlayRenderer::{new, render(&ActiveFrame, Option<&SubtitleOverlayFrame>, visible_area) -> SubtitleOverlayOutcome::{NothingToDraw, SkippedTooLarge, Drawn{bitmaps}, SkippedUploadFailed}, upload_stats() -> SubtitleOverlayUploadStats (поля wgpu + failed_uploads), has_picture_atlas, forget_uploaded, destroy}` + `SubtitleOverlayLayer` (FrameLayers::draw_overlays). Тот же overlay = revision + `shows_same_pixels_as` каждой картинки. Отказ загрузки (GL OOM, нарушенный контракт картинки) кадр НЕ роняет: пропуск до смены overlay + warn + счётчик.
- `diagnostics.rs`: `render_frame_stage_timings(&UiFrameTimings)` (нет у GLES2: encoder_creation, surface_view_creation, queue_submit, device_poll, pre_present_notify → 0), `render_frame_timing(&UiFrameTimings, renderer_elapsed)`, `video_pass_diagnostics(&RenderableFrame, &VideoDrawPlan)` (цветовой путь, `video_draw_rect_count` по плану, метки HDR-метаданных Confirmed/ReferenceDefault как wgpu), `frame_import_stats(&DmaBufImportCacheSnapshot)`. `VideoDrawPlan::{visible_rect_count, converts_hdr_to_sdr}`.
- `Presented` у `FrameCompositor` — только после успешного `eglSwapBuffers` (`composition.rs`, `presented?` до `Ok(Presented)`), на нём приложение пишет `video_render_acceptance`.

## Инструменты замера
- `docs/benchmarks/tools/local/evidence.py`: `SOFTWARE_PLANS`/`HARDWARE_PLANS`/`DMA_BUF_IMPORT_BACKENDS` по семействам, `fastiplayer_renderer_family(runtime) -> 'wgpu'|'gles2'|None` (None — нет следов или два семейства), HW GLES2 = `plan="vaapi-dmabuf-gles2"` + `VA-API EGL DMA-BUF`; `validate` пишет `renderer_family`. `t480s/collect.py` маркеры `rust_*_gles2`. Тесты: `test_protocol_v2.py::DecodePathTests`, `t480s/test_collect.py::PlaybackLogProofTests`. `agg_timing.py` правок не требует.
- ВАЖНО для 09: `video-vaapi` сейчас всегда пишет `backend_contract="VA-API Vulkan DMA-BUF"`; под GLES2 обязан `VA-API EGL DMA-BUF`, иначе evidence HW GLES2 не докажет.

## Тесты
- `GLES2_EGUI_REQUIRE_EGL=1 cargo test -p gles2-video --test overlay_readback` (13: смешивание по формуле, картинка×tint.a, загрузки, группы-uniform, clip, сброс состояния, порядок/серии, лимит, рост атласа, оба пути строк с хвостом 0xFF, malformed до GL, чужой контекст, пусто).
- `cargo test -p render-gles2-shell --test subtitle_overlay` — порт ВСЕХ 20 GPU-тестов wgpu (`tests/subtitle_overlay/{main,group_offsets,picture,edge_cut}.rs`, те же имена/ожидания; стенд — headless EGL, `AtlasSideLimit::AtMost` вместо урезанного лимита wgpu).
- Паритет: `cargo test -p render-wgpu-shell gles2_parity_tests::subtitles` — 7 сцен, max Δ 1 (RADV↔radeonsi и RADV↔llvmpipe). Для этого хелперы wgpu-тестов субтитров стали `pub(crate)` (только cfg(test)).
- Анти-вакуум (9 мутаций: группы, премультипликация, tint.a, сдвиг атласа, uniform-учёт, scissor, resident, clip, ревизия) — каждую ловит хотя бы один тест.
- Стенд: `cargo run --release -p render-gles2-shell --example subtitle_overlay_bench -- --frames 300` (1440×900, текст 2 строки и PGS 1200×160; upload первого кадра, GPU мс/кадр за вычетом заливки, сдвиг).
