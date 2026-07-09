# Audio Core / Concrete Audio

## Playback-rate audio contract (актуально с 2026-07-10)

- `audio-core` владеет нейтральными decoder/output/clock/tempo-контрактами и не знает CPAL, Signalsmith или timestretch. Concrete `crates/audio` владеет CPAL output и `AudioClock`; `crates/audio-signalsmith` владеет runtime DSP. `app-egui` связывает concrete factory с `player-core`.
- Runtime tempo backend — `audio_signalsmith::SignalsmithTempoProcessorFactory` поверх `signalsmith-stretch 0.1.3`. `audio-timestretch` остаётся только probe/evaluation host и не является runtime path. Guardrail запрещает прямые normal/dev/build зависимости `audio-core` и `player-core` на `timestretch`, `signalsmith-stretch`, `audio-timestretch` и `audio-signalsmith`.
- `AudioTempoDecodedMedia` всегда несёт `AudioTempoPcmFormat`; processor сравнивает его со своей конфигурацией и возвращает typed `PcmFormatMismatch`. Нельзя принимать PCM без доказанного sample rate/channel count.
- Нейтральный `AudioTempoProcessor` предоставляет `pcm_format`, `prime_decoded_history`, атомарный `set_segment`, `process_decoded_media_into`, `finish_stream_into` и `reset`. Produced PCM заимствует reusable output `Vec<f32>`, которым владеет caller; обязательной allocation внутри boundary на каждый packet нет.
- Accounting не смешивается: decoded-media input, реально produced output, actual processor-pending output, static input latency и static output latency — разные величины/оси. Static `output_latency` нельзя выдавать за actual pending после reset/flush.
- Signalsmith EOF lifecycle: сначала продвинуть processing time вызовом `process` с `input_latency` frames тишины, затем извлечь хвост через `flush` минимум на `output_latency`. Оба куска возвращаются одним учтённым результатом; после завершения actual pending равен нулю.
- Rate automation привязана к processing time. Если один input/EOF проход пересекает границу старого и нового DSP segment, adapter вызывает backend отдельными упорядоченными chunks, а report сохраняет ordered segment spans. Нельзя заменять это одним process-вызовом со средним ratio.
- `reset` очищает DSP/history/pending; prime-only finish также возвращает processor в чистое состояние. Waveform tests проверяют сохранность последних samples и переходы для 0.25x, 0.5x, 1x, 2x и 4x, а не только длину.
- Warmup при первом переходе с direct 1x в tempo path использует не больше 600 ms уже декодированного PCM. Priming не должен повторно отправлять этот PCM на output.
- Output policy 2A: `AudioOutputWriteIntent::DirectDecodedPcm` — чистый 1.0x путь без limiter/soft-clip; при совпадающем формате он bit-transparent. `TempoProcessed` явно применяет protection к DSP output. Если processor уже активен и rate вернулся к 1.0x, его хвост сохраняется как `TempoProcessed`; direct path возвращается после lifecycle reset/нового media.
- `AudioOutputClockTiming` — нейтральный snapshot audible output-clock position и submitted output end (ring tail плюс PCM, уже отданный callback, но ещё не дошедший до DAC). CPAL-типы не выходят из `crates/audio`.
- Pause boundary: `PlayerAudioOutput::pause_and_freeze_clock` атомарно сериализуется с callback consumer, возвращает `AudioOutputClockTiming` и замораживает clock. Resume компенсирует wall pause duration. Если устройство не поддерживает physical pause, concrete output использует logical silence gate; настоящая ошибка pause не маскируется.
- EOF считается завершённым только после submitted DAC tail, а не только после опустевшего ring buffer.
- Frame alignment остаётся обязательным: interleaved ring producer/callback работают только целыми frames; split frame может навсегда поменять каналы местами.
- Известное ограничение allocation: нейтральный tempo boundary переиспользует output buffer, но существующий concrete `AudioOutput::convert_decoder_samples_to_stream_layout` всё ещё создаёт `Vec` на write. Это отдельный future optimization, не нарушение tempo boundary.

## Проверки

- `cargo test -p audio-core`
- `cargo test -p audio-signalsmith`
- `cargo test -p audio`
- `cargo test -p audio-timestretch`
- Полный release gate дополнительно описан в `mem:task_completion` и `mem:player-core/playback-rate-contract-s32`.
