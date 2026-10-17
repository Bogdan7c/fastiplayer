# Смена формата decoded PCM посреди потока (AC-3 пакет C, 2026-10-16)

Решение владельца: смена decoded `AudioOutputSpec` (частота или layout) при активном output-е больше НЕ фатальна — output пересобирается без потери A/V sync. Механизм нейтральный (любой кодек), AC-3 adapter больше не держит формат первого кадра (`mem:audio/ac3-eac3-decoder`).

## Кто что владеет
- Детект: `ensure_audio_output_for_decoded_spec` (`session/audio_runtime.rs`): `audio_output_input_spec() != decoded spec` → `rebuild_audio_output_for_decoded_spec_change`. Тот же spec → no-op; output без spec-а (`install_audio_output_for_tests`) → no-op.
- Решение/порядок: `session/audio_spec_change.rs` (док модуля = полное обоснование). Порядок: проверить полный spec → `create_output(new spec, request по ActiveAudioOutputPlacement)` → `flush_audio_tempo_processor_for_eof` (DSP-хвост старого формата в СТАРЫЙ output) → `capture_audio_clock_mapping` (позиция стыка + `unplayed_output_tail()` из одного чтения) → `prefill_replacement_output_with_silence` (C3 helper, тишина = хвост) → стартовая позиция нового clock-а ДО play → play только если `pipeline.audio_output_is_playing()` → `pipeline.install_audio_output_after_spec_change` → `reset_audio_clock_sample` → запомнить placement + `publish_audio_output_route`.
- Pipeline: `pipeline/audio_output_spec_change.rs` — атомарная перестановка слота (`ReplacementOutputPlayback::{Started,NotStarted}`, `SpecChangeClockAnchor { output_clock_start, media_position, playback_rate }`, `mapping.reset_anchor` на ЯВНОЙ стартовой позиции, не «текущей»), `CapturedAudioClockMapping::unplayed_output_tail`, `audio_output_is_playing`.
- `pipeline/overlapped_audio_output.rs` — `OutputWithRetiringTail: PlayerAudioOutput` (декоратор в обычном слоте): write/clock/buffer level/health — только новый; play/pause/volume — оба; `clear_buffer_for_seek` закрывает старый; старый закрывается на записи, когда его `submitted_output_tail` == 0 или поток Failed; ошибки старого (play/pause) → warn + закрыть, не ошибка player-а. Повторная смена во время хвоста → вложенная цепочка, каждое звено уходит само.

## Инварианты
- Старый PCM не выбрасывается: старый поток доигрывает хвост параллельно с тишиной нового (сервер смешивает) → нет дыры (в отличие от C3, где хвост теряется) и первый PCM нового формата звучит в `стык + хвост × rate`.
- media = mapping(clock). С 2026-10-17 (пакет I) новый clock НЕ получает одиночный якорь с текущей скоростью: `SpecChangeClockAnchor.timeline = SpecChangeTimeline::ContinuePreviousMapping{previous_output_clock}` → `AudioClockMediaMapping::carry_onto_replacement_clock` переносит кусочную историю старого mapping-а (rate-сегменты ещё не проигранного хвоста) на ось нового clock-а; строится ДО `install_audio_clock` (тот сбрасывает mapping к одному якорю). `StartAtPlaybackRate(rate)` — только когда у старого output-а не было clock-а. Раньше смена rate меньше чем за хвост до смены формата сдвигала позицию на хвост×Δrate (тест `rate_change_inside_unplayed_tail_keeps_sync_across_format_change`, на старом коде 1.3 с вместо 1.2 с; unit `carried_mapping_keeps_tail_rate_segments_on_replacement_clock_axis`).
- Tempo ≠1x: старый processor финишируется в старый output и закрывается; новый создаётся по первому PCM нового формата без warmup-истории (как после seek). После tempo→1x (processor жив) — flush и дальше direct 1x.
- Устройство: `ActiveAudioOutputPlacement` (поле session) из фактического маршрута lazy init / recreate / rebuild. Selected → `SelectedOrSystemDefault`; уже на default → `SystemDefault` (нет автоворота на выбранное, нет повторного уведомления).
- Бюджет восстановления устройства не тратится и не сбрасывается. Событий нет (только `info!`); `AudioOutputReady` не публикуется.
- Ошибки: create/silence/play → `AudioDeviceUnavailable` → `disable_selected_audio_path` + `reanchor_no_audio_clock(позиция до стыка)` + обычный recoverable путь вызывающего кода (видео продолжает). Неполный spec / сбой DSP-flush → RuntimeError → fatal (политика tempo).

## Тесты
- `session/tests/audio_spec_change.rs` (fake `FormatSwitchingDecoder` по payload-у, `IdentityTempoFactory`): stereo→5.1→stereo в Playing (spec, тишина+samples в новом, старый без нового PCM, позиция без скачка, sync ±1 мс, без ошибок, device requests), контроль старого до конца хвоста, 48k→44.1k (8820 кадров тишины), identical spec, Paused (новый ждёт play, play продолжает оба), 1.5x (flush-маркер в старом, форматы processor-ов, sync = стык + 300 мс), отказ создания → degraded, после fallback на default остаётся default и бюджет 2 и 3 доступны, 4-й → AudioDisabled.
- `pipeline/tests/audio_output_spec_change.rs`: декоратор (маршрут PCM, release по хвосту/поломке/seek, play/pause обоих, ошибка старого не ошибка) + перестановка слота (якорь на 1.5x, NotStarted ждёт play).
- `session/tests/audio_runtime.rs::active_audio_output_rebuilds_for_same_count_layout_change` (заменил прежний fatal-тест).

## Проверки
`cargo test -p player-core --locked`, `cargo clippy -p player-core --all-targets --locked -- -D warnings`.

## Не сделано
- Живой прогон D6 (2026-10-17, release GUI, CPAL ALSA `default` на PipeWire null sink, `pw-record` монитора): вещательный TS одного мультиплекса AC-3 2.0→5.1→2.0 (чирп 200+40·t Гц в FL = метка времени медиа) — обе смены идут этим путём (`Audio output rebuilt for a mid-stream decoded format change`, старый отпускается `TailPlayed`), сдвиг время записи↔медиа 6 и 25 мс, паузы 12/17 мс (у ffmpeg на тех же стыках 0/15 мс — priming/padding самих потоков). На эксклюзивном ALSA hw второй поток может не открыться → degraded (то же допущение, что у C3 recreate) — не проверялось.
- С 2026-10-17 TS, склеенный `cat` из отдельных ffmpeg-выходов, больше не даёт `TracksChanged` (мягкий разрыв + одна шкала, `mem:mpeg-ts-demux/core`); если записи отличаются форматом звука на том же PID-е AC-3, смена пойдёт этим путём. Для чистой проверки смены формата по-прежнему удобен один mux (`cat` сырых `.ac3` → `ffmpeg -c:a copy -f mpegts`).
- Нет защиты от «дребезга» формата каждый packet (каждая смена = новый CPAL stream).
- Drop старого CPAL stream-а происходит в worker-потоке внутри записи PCM.
