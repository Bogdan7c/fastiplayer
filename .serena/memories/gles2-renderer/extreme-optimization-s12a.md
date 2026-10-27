# GLES2: экстремальная оптимизация (сессия 12a плана gles2-renderer, 2026-10-27)

Отчёт: `user/gles2-renderer/results/12a.md`. Решения владельца 62–68 README.

## Профиль T410 (фаза 1)
- Главная статья GPU — проход видео: 11,45 мс на кадр 1440×900 у SW 8 бит (mpv ~5). Причина по `INTEL_DEBUG=fs` — арифметика ДО выборок (зажимы, floor co-sited) и ветвление letterbox. На Ironlake любая арифметика до выборки дороже самой выборки: один зажим координат = +2,5 мс.
- `CLFLUSH` пути «0 копий» (12) — 61 % главного потока SW 1080p60 (~1,65 мс/кадр); решение 62: не переносить (CPU процесса не меняется).
- В фуллскрине кадров «только интерфейс» нет; таймер-запросов GPU у crocus нет — стенд `crates/gles2-video/examples/video_shader_cost_probe.rs` (серия проходов + glFinish).
- `perf` установлен на T410 (`perf_event_paranoid=1`).

## Принятые трюки (коммиты)
- C1 «дешёвый пиксель» (`7c621835`, gles2-video): координаты выборок в вершинах (`u_luma_uv_scale`/`u_chroma_uv_scale`), co-sited 4:2:0 — `GL_NEAREST` (`ChromaFilter`, задаётся в `bind_plane_textures`), 4:2:2 считает X в шейдере (`CHROMA_CO_SITED_X_IN_SHADER`); нет ветвления letterbox — scissor по `geometry::picture_pixel_rect`, полосы заливает glClear; зажим у края только вариант `SamplingEdge::ShaderClamp` (драйверы без unpack_subimage); счётчик `VideoLayerStats::shaded_video_pixels`. T410: SW 1080p60 Render/3D 63 → 30 %, HW 720p60 50–55 → 59 к/с, HW 1080p30 28,5 → 29,7, HDR 60 → 45 %.
- Слоты zero-copy VA следуют за поднятым пулом поверхностей (`c312176c`, `mem:video-vaapi/core`).
- 10/12 бит без копии (`027cc954`): GR88, `PlaneTextureFormat::RgExt`, `BytePairChannels::{LuminanceAlpha, RedGreen}` (`#define BYTE_PAIRS_IN_RED_GREEN`), выбор в `plan_frame_color(layout, texture_format, color)`; `HighBitFrameWithoutBytePairTextures`. T410 HEVC Main10 HDR 1080p30: CPU 173,6 → 161,5 %.
- Пул VA «авто» (`07bb7265`, схема v27): `0 = авто`, `player_core::AutomaticSurfacePool`, `app-egui::resource_driver::hardware_surface_pool_default_for` (решение 69 `08b331f5`: GLES2 тоже 24 — пул 16 давал `ui_model_changed` на половине кадров HW 720p60, GPU 86 → 94 %; общий бэклог п. 17), `fastiplayer_config::MIN_AUTOMATIC_SURFACE_POOL_FRAMES = 16`; тесты маршрутов настроек — `fastiplayer-settings/src/routing/tests.rs` (`c3aad340`).
- Частичный показ (`8c39e716`): `gles2-egui::damage` (`DamageHistory`, `RepaintPlan`, `FullRepaintReason`, `PartialPresentStats`), `EglDriver::{query_buffer_age, swap_buffers_with_damage}` (`context/safety/damage_swap.rs`), урон UI тайлами 16 px (`painter/ui_damage.rs`), граница `ActiveFrame::repaint_area()` — любой слой обязан рисовать только внутри; `FrameCompositor::render_with_layers_damage`; `gles2-video/src/repaint_clip.rs`; шелл `shell/layers_damage.rs`; offscreen-кольцо `Gles2Context::offscreen_with_back_buffers` для тестов. T410 Wayland: buffer age есть, KHR damage swap; пауза + наведение GPU 44 → 14,5 %. Урон UI считается только когда частичный показ возможен (`UiDamageTracking::{Track, Skip}`, `FullRepaintReason::UiBaselineUnknown`, `a1b5369a`): иначе тайлы стоили 25 % главного потока HW 720p30. Сводка PartialPresentStats пишется в `Drop` рендера — при SIGTERM не видна.
- HEVC на ≤4 логических CPU: логические + 2 потока (`255d2e55`, `CONSTRAINED_HOST_HEVC_EXTRA_THREADS`); ступенька лесенки HEVC 1080p60 25 Мбит/с — у нас 0 потерь, VLC 16–19.

## Стенды и скрипты
Скрипты профиля/A/B T410 — в scratchpad сессии (profile_run.sh: CPU по потокам, Render/3D, RSS, DRM resident; считать кадры только по строкам TRACE «render frame stage timings» — DEBUG «slow render frame…» тоже содержит эту фразу).
