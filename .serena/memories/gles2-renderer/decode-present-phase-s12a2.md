# Подсессия 12a.2: такт декодера по показу, бюджет замка, i965 (2026-10-28, DONE)

Отчёт: `user/gles2-renderer/results/12a.2.md`, решения владельца 77–81 README плана. Предыдущая: `mem:gles2-renderer/hw-near-miss-s12a1`.

## Корень опозданий HW 1080p30 на T410
- На 1080p30 декодер VA-API работает почти без пауз: занят 74 %, медианная пауза между `decode()` 0,1 мс, запас 300–530 мс. Ironlake декодирует 1080p чуть быстрее 30 к/с, поэтому окно decode-ahead его почти не тормозит.
- В общей FIFO-очереди GPU (`mem:gles2-renderer/hw-near-miss-s12a1`) почти всегда стоит его картинка (~27 мс), и проход показа ждёт за ней. Сдвиг фазы не помог бы — нужен такт.

## Архитектура (решения 77, 78)
- `video_core::PresentPassSignal` / `PresentPassSubscription` (`video-core/src/decoder_thread/present_pass.rs`): у рендера неблокирующий звонок со слиянием повторов, у каждой подписки свой канал ёмкости 1. Мёртвые подписки выбрасываются при звонке; отравление — типизированный исход.
- `RenderShell::present_pass_signal() -> Option<_>`: GLES2 — `Some` (звонок в `complete_presented_frame` после fence, `frame_fence.rs::signal_present_pass_submitted`, счётчик `present_pass_signals` = `frames_presented`); wgpu — `None`. Conformance-проверка 12 `PresentPassSignalFollowsPresentedFrames`.
- app-egui: `video_pipeline_candidate::DecoderRendererLink { family, present_pass_signal }` — вход `start_decoder_backend`, `vaapi_factory_for_renderer`, `RendererVideoImportTarget.renderer_link`. Программный декодер сигнала не получает.
- video-vaapi:
  - `VaapiVideoBackendFactory::with_present_pass_signal`: каждый запуск подписывается заново;
  - `VideoDecodeThread::new_with_present_pass_pacing` (`decoder_thread/start.rs`);
  - политика `decoder_thread/present_pacing.rs`: запас `decoded_frames_ahead` (зарегистрированные в пуле + «в полёте») ≥ `PRESENT_PACING_MIN_DECODED_FRAMES_AHEAD` = 4 — следующий пакет ждёт сигнала; одна подача на один показ;
  - запас меньше — подача сразу, старый сигнал выбрасывается;
  - control прерывает ожидание; `PRESENT_PULSE_WAIT_LIMIT` = 100 мс; разрыв сигнала выключает такт;
  - ожидание идёт до вынимания пакета (`await_present_pass_permit` перед `select!` в `runtime_loop.rs`), поэтому flush сбрасывает пакеты обычным путём.
- `decoder_thread.rs` — 798 строк при лимите 800: новое — только в дочерние модули.

## Бюджет замка (решение 81)
- С тактом декодер просыпается ровно тогда, когда приложение ищет следующий кадр, и `try_lock` пула ресурсов отвечал `Busy`. Итог — повтор кадра, потом пропуск: 780M GLES2 1080p60, 1–3 за 20 с. Доказано временной трассой Busy.
- `decoder_thread/resource_provider/lock_budget.rs::lock_within_budget` + `RESOURCE_LOOKUP_LOCK_BUDGET` = 250 мкс во всех `try_resource_*_lookup`, для всех рендеров. Дольше — по-прежнему `Busy`.
- Гипотеза «доделывать кадры в полёте перед ожиданием» не помогла и откачена.

## i965 (решение 80)
- На каждую картинку драйвер сам отображает в CPU (`dri_bo_map`) три батч-буфера по 512 КиБ и `avc_it_command_mb_info` (~1 МиБ на 1080p). libdrm переиспользует их, когда GPU свободен, и `SET_DOMAIN(CPU)` даёт `drm_clflush_sg`.
- Буферы cros-codecs ни при чём: параметры i965 держит в `malloc`, слайс — один `pwrite`.
- Со стороны клиента рычаг только «держать GPU занятым», а это противоречит такту. Поток декода 720p30 — 7,0 %.

## Числа (T410 ABBA, 20 с)
- 1080p30: опоздания 28–41 → 0 (Wayland и XWayland), 30,05 кадра/с, CPU тот же.
- 720p60: 33-мс пропуски 0–1 → 0.
- 720p30: CPU тот же.
- Цена: 720p60 Wayland +0,6–0,8 п.п. CPU процесса (главный поток +0,5), профиль не снимался.
- 780M GLES2: 0 пропусков на 1080p60/4K60, CPU +0,3–0,5 п.п.; wgpu без изменений.

## Метод и инструменты
- Каденс: `docs/benchmarks/tools/local/cadence.py` (`late_frames` у diagnostic Fastiplayer, описательно, решение 79) по событиям `current video frame submitted to surface`.
- Аппаратный сценарий `scripts/media-regression.sh --scenario h264-mp4-vaapi-present-pacing --path <H.264 MP4 ≥40 пакетов>`. На T410 тестовые бинари копируются с основной машины (`cargo test --no-run`).
- Разбор повторов: `RUST_LOG=player_core::session::tick::presentation_scheduler=trace` («frame selected» с `target_ms`). Повтор планировщик не логирует, Busy-поиск в обычной трассе виден только у медленных кадров.
- Тесты инструментов (`python3 -m unittest discover -s docs/benchmarks/tools/local -p 'test_*.py'`) в CI не входят. `test_make_matrix` был красным с 12a и исправлен.
- `surface-pool` требует файл ≥ 10 с (720p-фикстура `test-assets/H264` — 8 с).
