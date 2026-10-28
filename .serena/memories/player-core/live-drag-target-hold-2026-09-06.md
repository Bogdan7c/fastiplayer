# Live drag: bounded target hold (2026-09-06)

Связано: `mem:player-core/core`, `mem:player-core/scrub-commit-policy-s09`.

## Причина и инварианты

Длинный LiveScrub мог продвинуть картинку до EOF и завершить release ошибкой SeekTimeout. Три взаимодействовавших механизма: active seek обходил presentation queue admission (oldest landing frames вытеснялись), обычная audio-stall recovery потребляла queued frames при намеренно paused audio, а tiny forward extension помечал уже подходящий current-generation presented frame stale и заставлял scheduler съесть следующий кадр.

- `session/tick/video_decoder_io/present_admission.rs` владеет только read-only расчётом receive/send budgets. LiveScrub использует свободные места bounded presentation queue; unread decoder output остаётся в decoder channel, backpressure сохраняется. Остальные seek modes сохраняют прежний fast-preroll bypass. Pipeline остаётся владельцем queue/resources, decoder I/O — приёма/отправки и release ошибок.
- `present_live_scrub_preroll_roll` удерживает policy-qualified presented landing до изменения target или завершения commit. Эта ветка идёт до обычного audio-stall recovery. Resume frames остаются queued; one-shot seek и нормальная playback stall recovery не меняются.
- `try_extend_active_live_scrub_landing_forward` после обновления target/trace повторно пропускает текущий presented frame через существующий generation/landing guard `note_presented_frame_for_seek`. Подходящий кадр сохраняет target evidence без следующего forced present; pre-target кадр по-прежнему не открывает landing gate.
- EndScrub policy, visible-vs-latest resolution, worker receipt, audio readiness, flush/generation/release ownership и cold backward/far-forward route не изменены.
- Размер старого decoder I/O уменьшён 865→836 строк; module-size snapshot ratcheted вниз. Coverage policy/baseline/исключения не менялись.

## Уточнения 12b gles2-renderer (2026-10-28)
- Продление live-scrub прохода вперёд (`seek_start.rs::try_extend_active_live_scrub_landing_forward`, кап `LIVE_SCRUB_FORWARD_EXTENSION_MAX` 3 с) меряет дистанцию от прогресса decoder-а (`PlaybackPipeline::latest_video_decoder_output_pts`, PTS последнего выданного кадра эпохи), а не от прошлой цели указателя; без кадров в проходе — от прошлой цели. Раньше цепочка шагов < 3 с тянула HW-проход от старого keyframe бесконечно (T410: превью 44 с при курсоре 58,5 с, отпускание садило на 49 с). Тесты `scrub.rs::live_scrub_slow_decoder_far_from_pointer_takes_cold_route`, `live_scrub_fast_decoder_keeping_up_extends_without_second_demux_seek`.
- Новый scrub-жест, пока seek прошлого жеста ещё садится (публичное `Scrubbing`), наследует его resume intent (`scrub_orchestration.rs::playback_state_for_new_simple_scrub` → `in_flight_seek_resume_intent()`); раньше `Scrubbing` → «был на паузе», и два drag/клика подряд ставили видео на паузу. Тесты в `session/tests/seek_resume_intent.rs`.

## Регрессии и аппаратное evidence

`session/tests/scrub_hold.rs` проходит public commands → scripted demux packets → fake decode-on-send → actual present frame: десятисекундное удержание и bounded send/release/resume; 200 tiny forward updates с ближайшим frame и неизменной generation, затем backward cold route; audio readiness удерживает release до реального output.play и дальнейшего video presentation. Все три теста воспроизводят failure со старым production code (видимый кадр ~27.84 s вместо 8 s) и проходят с исправлением. Existing scrub, seek, worker-receipt, decoder error/resource regressions сохраняются.

Реальный разрешённый H.264 1080p24 + AAC48k trailer (~52.2s), VA-API NV12 DMA-BUF/Vulkan: fullscreen drag x780→1270→665→1060 за3/3.5/2.5s после seek~17.929s. Release visible target28.916s → presented28.916s за64ms → commit/Playing65ms → дальнейший playback. Изолированный профиль, media и диагностические артефакты вне Git. Окончательные exact-SHA CI/verification отражаются в отчёте задачи; эти timings — аппаратная проверка production fix, не performance promise.
