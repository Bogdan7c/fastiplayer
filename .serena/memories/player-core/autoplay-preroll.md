# Player Core Autoplay Preroll

- `PlayerSession` owns autoplay preroll policy. `begin_autoplay_preroll()` enters `PlaybackState::Buffering`; `finish_autoplay_preroll_if_ready()` is the only normal transition to `Playing` after the audio/video gates are satisfied.
- Audio readiness stays owned by the audio runtime boundary. Video readiness must not read pipeline storage directly; use `PlaybackPipeline` boundary methods.
- Video-only autoplay is ready when there is no selected video track, or when the pipeline already has either a current present frame or at least one queued decoded/presentation frame. This matters for H.264/MP4 B-frame startup where the first decoded display frame may be tens of milliseconds after zero; while still in `Buffering`, the no-audio monotonic media clock is not allowed to advance, so requiring an already-presented frame can deadlock startup.
- Focused coverage lives in `crates/player-core/src/session/tests/playback.rs`: `video_only_autoplay_keeps_present_frame_gate` and `video_only_autoplay_accepts_queued_preroll_frame`.


## Кадр-постер на паузе (G5, 2026-10-11)

- Новое media ставится сразу в `Paused`, а декодер получает пакеты только в `Playing/Buffering/Seeking/Draining`, поэтому пауза до первого кадра (старт с `player.start_paused`, CLI-ссылка, resume у конца по B2 → `Continue`, Pause во время preroll) оставляла экран чёрным. Из середины кадр был лишь потому, что восстановление позиции — обычный seek.
- Владелец — `session/paused_poster_frame.rs`: `pause()` после перехода в `Paused` вызывает `request_paused_poster_frame_if_missing()`; если выбран видеотрек, нет present-кадра, нет активного seek/scrub, таймлайн static и seekable — запускается обычный `seek` на текущую позицию (resume intent = Pause), он декодирует кадр и возвращает `Paused`. Отдельного режима «декод на паузе» нет. Отказ seek-а логируется (warn), не ошибка команды.
- Во время постера `playback_state` кратко `Scrubbing/Seeking` (как при restore-seek). Для unit-тестов, где Pause проверяется без кадра, фикстурам нужен present-кадр (`show_frame_on_screen_for_tests` в worker-тестах, `set_present_video_frame` в session-тестах). Тесты постера: `session/tests/paused_poster_frame.rs`.
- Не покрыто: live и несикабельный static без постера; install сразу в `Paused` без последующего Pause-intent (все app-пути шлют intent после install).

## Audio-aware demux starvation recovery (2026-08-27)

Temporary installed-demux starvation with selected audio now proactively freezes output when usable runway cannot cover remaining retry wait plus the canonically sanitized audio low-water scheduler/callback margin, independently of queued video. Remaining wait uses a fresh post-demux decision timestamp. Any exact current pending retry, including chained TUA after a recovered packet, blocks resume until the next matching accepted source event clears the source-wait fence; then the existing audio+video preroll gate is authoritative. Buffering resume publishes `Playing`/`AudioPlaybackResumed` only after successful output play and clock anchoring; recoverable play failure remains Buffering and is retried by the ordinary preroll lifecycle. Full invariants and tests: `mem:player-core/audio-starvation-buffering-resume-2026-08-27`.
