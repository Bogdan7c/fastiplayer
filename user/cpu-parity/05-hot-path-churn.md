# Сессия 05: россыпь на горячем пути

Приложи этот файл и скопируй абзац ниже в сообщение.

> Выполни сессию 05 плана `user/cpu-parity/`: убери лишние клоны, аллокации, dup/fstat и пересоздание GPU-объектов на горячем пути кадра и тика. Требует сессий 03 и 04. Сначала прочитай `user/cpu-parity/README.md`, `results/00.md`, `results/03.md`, `results/04.md`. Каждая правка — со счётчиком или тестом, доказывающим, что работа исчезла.

## Что найдено (сессия 00, чтение кода)

| Что | Где | Сколько |
|---|---|---|
| `PlayerSnapshot` клонируется дважды на vsync (`latest_snapshot` + `last_player_snapshot = clone`) | `crates/player-core/src/worker/handle.rs:298-310`, `crates/app-egui/src/state.rs:752-759` | 2 глубоких клона × 60/с |
| Полная пересборка `PlayerSnapshot` после каждого тика и каждой команды, без dirty-проверки (`source_label`, `media_title`, `tracks`, `media_info`, `queues`, `diagnostics`) | `crates/player-core/src/worker/runtime_publish.rs:236`, `session/snapshot_builder.rs:40-71` | каждый тик |
| MPRIS/desktop-снимок: `DesktopSnapshotView` строится и клонируется трижды на кадр | `crates/app-egui/src/playlist_runtime/desktop_transport.rs:195-300`, `crates/desktop-integration/src/runtime.rs:31-43` | 3 клона × 60/с |
| Render-feedback семплы `try_send` на каждый vsync (acquire, timing, resource-lock) — будят worker | `crates/player-core/src/render_lease_bridge.rs:289, 381, 387` | 2–4 сообщения × 60/с |
| Bind group + `write_buffer` uniform на каждый кадр (NV12/P010/host YUV) | `crates/render-wgpu-video/src/video/nv12_renderer.rs:190-200`, `p010_renderer.rs:338`, `host_yuv420_renderer.rs:272,336` | 60/с |
| `RenderDiagnostics` клон на каждый кадр; ~15 `Instant::now()` на кадр ради lazy-логов | `crates/render-wgpu-shell/src/shell.rs:437`, `crates/app-egui/src/frame_prepare.rs` | 60/с |
| `#[instrument]` уровня INFO на `render_frame` и `render_ui` — span на каждый vsync | `frame_prepare.rs:826`, `state/ui_runtime.rs:168` | 2 span × 60/с |
| Linear resampler аллоцирует два `Vec` на вызов (только при несовпадении частот) | `crates/audio/src/output/resampler.rs:46-70` | каждый блок |
| `smithay-clipboard` поток 0,15–0,35 % в простое | зависимость winit/egui | постоянно |

## Архитектура

- Снимки: владелец — `player-core::worker` (publish по dirty-флагу: снимок пересобирается, когда изменились его составляющие; строки/вектора — `Arc`, а не `String`/`Vec` клоны; UI забирает `Arc<PlayerSnapshot>` без второго клона). Контракт `latest_snapshot()` сохраняет семантику «последний опубликованный».
- MPRIS: владелец — `app-egui::playlist_runtime::desktop_transport` — публиковать только при изменении (позиция уже не шлёт D-Bus; теперь и клоны не делать).
- Render-feedback: владелец — `player-core::render_lease_bridge` — агрегировать семплы на стороне UI и отправлять раз в тик/при изменении; worker не должен просыпаться от телеметрии (после сессии 02 это важно).
- Рендер: uniform обновлять только при изменении (viewport/ориентация/цвет), bind group кэшировать по `(texture views identity, uniform slot)` у рендерера-владельца; после сессии 03 views стабильны по поверхности пула — кэш bind group становится естественным.
- Инструментирование: span-ы на горячих функциях — уровень TRACE/DEBUG или `skip_all` + `level = "trace"`; стадийные таймеры оставить (они нужны `agg_timing.py`), но собирать в одну структуру без дублирующих `Instant::now()`.
- Clipboard-поток: сначала выяснить причину (strace/профиль); если это poll внутри зависимости — зафиксировать в бэклоге, не патчить вслепую.

Memories: `mem:player-core/core`, `mem:app-egui/playlist-desktop-transport-s18b`, `mem:render-video/core`, `mem:app-egui/state-split`.

## Инварианты

1. UI всегда видит самый свежий снимок не позже следующего кадра; «dirty» не теряет изменения (любое поле, которое читает UI, помечает снимок грязным).
2. Диагностика/телеметрия сохраняет все поля; только частота доставки меняется.
3. Семантика `Busy`/`Missing`/release path и worst-latency учёт не меняются.
4. Нет новых `unsafe`.

## Тесты

- Снимок: N тиков без изменений → 0 публикаций/клонов (счётчик), изменение любого отображаемого поля → 1 публикация; UI получает `Arc` без клона (тест на `Arc::ptr_eq`).
- MPRIS: позиция меняется → `publish_snapshot` не вызывается; метаданные/состояние меняются → вызывается один раз.
- Рендер: fake device считает `create_bind_group`/`write_buffer`: серия кадров с теми же views и viewport → 0 после первого.
- Тесты должны падать на текущем коде.

## Замер и критерий выхода

- До/после `run_pass.py` HW 1080p60 и SW 1080p60; `agg_timing.py`. Ожидание: главный поток и worker теряют остаточные десятые доли процента; аллокации на кадр (посчитать профилем samply/perf по `malloc`) — ноль в установившемся режиме.
- Quality-gate README; MPRIS-клиенты (plasma media controller) показывают актуальные данные.
- Отчёт `results/05.md`, обновить затронутые memories.
