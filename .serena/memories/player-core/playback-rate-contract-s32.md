# Playback Rate Contract (S32-S40 lifecycle hardening)

## Публичная модель и scope

- Playback rate V1 использует существующую async очередь `PlayerCommand`; отдельный request/reply API не добавляется.
- Публичные значения: validated `PlaybackRate` в диапазоне 0.25x..=4.0x, `PlayerCommand::SetPlaybackRate`, `PlayerSnapshot.playback_rate`. Новый media всегда начинает с 1.0x.
- Rate не хранится в TOML, Settings, history/startup restore и не публикуется через desktop/MPRIS. `frame-server-core` не владеет current rate, audio mapping или concrete tempo lifecycle.
- Set rate разрешён только в `Playing` и `Paused`; остальные состояния возвращают semantic `PlayerCommandOutcome::Rejected`, не fatal runtime error.

## Ownership и timing boundary

- `PlaybackPipeline` владеет активным tempo processor, bounded 600 ms direct-PCM warmup history и piecewise `AudioClockMediaMapping`. `PlayerAudioClock` остаётся output-time clock; media-time не переносится в `audio-core`.
- Mapping хранит старые tempo segments, submitted device/DAC tail и будущие processor-output spans. Он выполняет оба преобразования: output-clock -> media-time и media deadline -> output/wall delay. После merge соседних equal-rate spans media cursor всегда пересчитывается от канонического объединённого segment-а: нельзя суммировать отдельно округлённые nanosecond deltas. Exact continuity на границе segment-ов остаётся обязательным invariant.
- Inverse timing возвращает минимальный delay, для которого mapped media position уже не меньше deadline; target output считается абсолютно от начала выбранного segment-а. Для положительного delay предыдущий output tick (delay - 1 ns) ещё не должен достигать deadline. Exact equality с deadline не требуется, потому что fast rate может перепрыгивать отдельные nanosecond значения.
- Scheduler вызывает session/pipeline intent API для media deadline. Он не угадывает timing через `has_audio_clock` и не знает CPAL. `MonotonicMediaClockAnchor` аналогично владеет forward projection и inverse deadline для no-audio пути и считает оба от своего исходного anchor, не от округлённого текущего snapshot. Инварианты: 1 s media при 4x даёт примерно 250 ms wall, при 0.5x — примерно 2 s; rate-change tail сначала доигрывается по старому segment.
- `CapturedAudioClockMapping` получает audible position и submitted output end одним snapshot, поэтому SetRate не смешивает значения из разных callback моментов.
- Публикация clock sample отделена от явного re-anchor. Обычный `update_position_for_tick` не создаёт новый no-audio anchor на другом `Instant::now()`.

## Rate-change и error policy 1A

- Segment id сначала предлагается, а commit rate/mapping/snapshot выполняется только после успешного tempo prepare/`set_segment`.
- Если для выбранной audio track отсутствует output/clock, PCM format ещё неизвестен или backend отклонил rate, команда атомарно возвращает `PlayerCommandReject::PlaybackRateAudioTempoRejected` с typed `PlaybackRateAudioTempoRejectReason`. Старые rate, segment id, mapping, processor, output и playback state сохраняются.
- Video-only media может менять rate через no-audio clock. Но потеря audio в media с выбранной audio track не считается video-only fallback: нельзя молча отключить audio или вернуть Applied.
- Ошибка DSP process/EOF после commit — runtime fatal: session переходит в `Failed`, а audio не отключается молча.
- Первый переход direct 1x -> tempo создаёт Signalsmith processor, prime-ит не более 600 ms history и отбрасывает только duplicate warmup output. Factory/prime failure возвращает history и segment proposal, поэтому retry возможен.
- Clean startup 1x идёт как `DirectDecodedPcm` без protection. После появления processor возврат к 1x сохраняет DSP continuity до EOF/reset и пишет `TempoProcessed`.

## Accelerated video overload policy (choice 1)

- Exact requested audio/media clock remains authoritative at `>1x`; scheduler may reduce video smoothness only through a codec-safe compressed backlog recovery. It must not silently slow audio, pause playback, or report a fake applied rate.
- `PlaybackPipeline` owns a two-phase AV1/VP9 recovery state machine. Existing pending packets remain decoder runway while new video packets are staged. Only a proven keyframe for the selected track/current seek generation commits the switch; packet metadata is preserved and decoder/generation are not flushed or recreated.
- EOF, bounded scan rollback, and a successfully committed downshift to `<=1x` append staged continuation after old pending FIFO. A tempo-backend rejection happens before reconciliation and preserves old rate, scan and staging atomically. Seek/media/track/generation/decoder replacement cancel staging; Pause and accelerated-to-accelerated transitions preserve it.
- Default allocation guard is 512 compressed packets plus 32 MiB retained payload. The target HDR AV1 4K60 asset has a measured maximum 420-frame / 20.63 MiB GOP. Packet- and byte-limit rollback are typed; current staging packet/byte depth is in queue diagnostics; dense scan packet telemetry uses a scalar rather than growing `PlayerTickResult.demuxed_packets`.
- No-flush recovery is deliberately restricted to AV1/VP9. H.264/H.265/VP8 remain ordinary FIFO because generic `PacketKeyframe` does not prove all required random-access/leading-picture semantics. `1x` and slow playback never start the recovery scan.

## Pause lifecycle

- Pause сначала атомарно вызывает output `pause_and_freeze_clock`, затем вычисляет media position из того же captured timing и только после успеха публикует `Paused`.
- `SetPlaybackRate` в Paused использует frozen audio-backed position, не snapshot последнего tick. Resume продолжает с frozen anchor без wall-time скачка.
- Regression: Play -> audio clock advance -> Pause -> SetPlaybackRate -> Play сохраняет позицию и корректно продолжает новый segment.

## Signalsmith lifecycle и tests

- Runtime backend — `audio-signalsmith` / `signalsmith-stretch 0.1.3`; `audio-timestretch` probe-only. Полный neutral/EOF/segment contract находится в `mem:audio/core`.
- Direct, tempo packet и tempo EOF PCM используют единый `AudioOutputRoutingStatus` поверх neutral `AudioOutputWriteReport`. Player-core не сравнивает input scalar count с post-conversion output count: channel conversion/resampling могут законно менять их. `AudioOutputAbsent`, typed write failure, complete и real partial paths остаются различимыми; real partial является fatal.
- Decoded PCM layout переносится единым `AudioOutputSpec`; output и 600 ms warmup history принимают полный sample-rate/layout spec. Смена layout при том же count отвергается до mutation и становится fatal runtime incompatibility, а не video-only fallback. Tempo DSP по-прежнему зависит только от rate/count и сохраняет lane order; positional 5.1→stereo matrix принадлежит concrete `audio`, не scheduler/player-core. Matching-count direct 1x остаётся bit-transparent, а multichannel downmix является явным преобразованием с fixed headroom и LFE=0.
- Focused tests находятся в `crates/audio-core/src/tempo/tests.rs`, `crates/audio-signalsmith/src/adapter/tests/`, `crates/audio/src/{clock,output}/tests/`, `crates/player-core/src/pipeline/tests.rs`, `crates/player-core/src/session/tests/{audio_runtime,playback_rate,eof_drain}.rs`, tick/scheduler tests и app-egui control/state tests.
- Основной gate: `cargo test -p audio-core`, `cargo test -p audio-signalsmith`, `cargo test -p audio`, `cargo test -p player-core`, `cargo test -p app-egui`, `cargo check --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --all --check`, `scripts/check-refactor-guardrails.py`, `git diff --check`.
- Guardrail regression: `python -m unittest scripts.tests.test_check_refactor_guardrails`.
- S39/S40 нельзя закрывать только автоматическими тестами: manual release smoke для audio/video-only 0.25x/0.5x/1x/2x/4x, media reset, pause/rate-change, EOF и repeated changes остаётся обязательным до фактического выполнения.
