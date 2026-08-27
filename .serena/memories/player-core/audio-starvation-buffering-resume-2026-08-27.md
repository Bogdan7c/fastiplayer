# Audio-aware demux starvation buffering/resume (2026-08-27)

## Причина

Installed temporary demux readiness раньше входила в Buffering только после полного drain audio и video. Для A/V с audio presentation master это было слишком поздно: output ring уже писал silence, пока video present queue оставалась непустой. Первый routed packet затем напрямую возвращал Playing, обходя общий autoplay preroll gate.

## Ownership и lifecycle

- `session/demux_retry.rs` владеет exact media-instance + seek-generation retry fence и typed причиной входа в Buffering.
- Для selected audio proactive starvation risk определяется как empty pending compressed audio и `usable output runway <= remaining retry wait + scheduler/callback margin`. Remaining wait считается от одного fresh owner timestamp, захваченного непосредственно перед runway decision после demux scheduling, а не от раннего `tick_context.now`. Margin берётся из существующего `PlayerTickConfig::audio_demux_low_water_mark_ms` через canonical `sanitize_audio_demux_low_water_mark`; negative/NaN/+inf используют общий default 100 ms. Отсутствие output означает zero runway. Наличие queued video больше не блокирует Buffering.
- Без selected audio сохранён прежний full video downstream drain gate.
- `session/runtime_control.rs::freeze_playback_for_demux_buffering` вызывает neutral `PlayerAudioOutput::pause_and_freeze_clock`, публикует frozen position и намеренно сохраняет video presentation queue. Absent output является no-op без ложной device error. Pause error становится typed `RuntimeError`; tick отмечает fatal, не публикуя partial Buffering.
- Принятый current-pipeline demux packet только снимает retry/source-wait. Он не меняет playback state.
- Единственный обычный resume из Buffering остаётся `finish_autoplay_preroll_if_ready`: любой exact current pending demux retry блокирует resume, включая chained TUA с новой записью `entered_buffering=false`; только следующий matching accepted source event снимает fence. Затем выбранный audio требует decoder + output + configured preroll (production default 50 ms), video требует present или queued frame. Resume atomic: output `play()` и clock anchors завершаются до `Playing`/`AudioPlaybackResumed`. Play failure остаётся recoverable typed `RuntimeError`, сохраняет Buffering/frozen clock и допускает повтор на следующем обычном preroll tick; одинаковая persistent ошибка не дублирует WARN/event.
- Paused intent, stale generation/media instance, EOF/error/TracksChanged и playback-window rejected packets не дают ложного resume.

## Diagnostics

`session/audio_starvation.rs` отделяет классификацию starvation diagnostics от orchestration в `audio_runtime.rs`; размер файлов не является частью контракта. Legacy audio-clock counter трактуется только как доказанный callback silence-padding при пустом output ring, а не native CPAL xrun. Low buffer без новой callback delta логируется как `low_buffer_starvation_risk_only`; доказанный случай — `output_ring_underrun_proven_by_silence_padding`.

## Functional proof

`session/tests/demux_retry.rs::audio_starvation_buffers_and_resumes_only_after_full_av_preroll` проходит production-like `PlayerSession::tick`: Playing A/V, selected audio, future queued video, temporary demux unavailability, insufficient audio runway -> one pause/freeze + Buffering + preserved video queue/frozen clock; recovered video alone и 49 ms audio не resume; 50 ms + video ready дают один atomic play; follow-up packet не повторяет resume. Отдельные tests закрепляют 5 ms runway против 10 ms retry, достаточный runway, public hint bounds 1 ms..60 s и recoverable play-error retry без ложных success state/events.

Focused matrix также закрепляет video-only/no-audio, paused intent, absent output, typed pause/play errors, stale seek generation, EOF drain, playback-window rejection и стабильность underrun counter во время logical pause.

Финальный независимый audio review подтвердил lifecycle fix. Durable verification: focused `cargo test --locked --all-features -p player-core session::tests::demux_retry`, полный `cargo test --locked --all-features -p player-core`, strict `cargo clippy --locked --all-features --all-targets -p player-core -- -D warnings`, formatting/diff checks и Serena diagnostics. Абсолютное число тестов не является контрактом: focused и full suites растут вместе с соседними модулями; результат нужно читать из конкретного CI/reviewer прогона.
