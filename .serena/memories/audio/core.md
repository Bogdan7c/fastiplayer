# Audio Core / Concrete Audio

## Playback-rate audio contract (актуально с 2026-07-10)

- `audio-core` владеет нейтральными decoder/output/clock/tempo-контрактами и не знает CPAL, Signalsmith или timestretch. Concrete `crates/audio` владеет CPAL output и `AudioClock`; `crates/audio-signalsmith` владеет runtime DSP. `app-egui` связывает concrete factory с `player-core`.
- Runtime tempo backend — `audio_signalsmith::SignalsmithTempoProcessorFactory` поверх `signalsmith-stretch 0.1.3`. `audio-timestretch` остаётся только probe/evaluation host и не является runtime path. Guardrail запрещает прямые normal/dev/build зависимости `audio-core` и `player-core` на `timestretch`, `signalsmith-stretch`, `audio-timestretch` и `audio-signalsmith`.
- `AudioTempoDecodedMedia` всегда несёт `AudioTempoPcmFormat`; processor сравнивает его со своей конфигурацией и возвращает typed `PcmFormatMismatch`. Нельзя принимать PCM без доказанного sample rate/channel count.
- Нейтральный `AudioTempoProcessor` предоставляет `pcm_format`, `prime_decoded_history`, атомарный `set_segment`, `process_decoded_media_into`, `finish_stream_into` и `reset`. Produced PCM заимствует reusable output `Vec<f32>`, которым владеет caller; обязательной allocation внутри boundary на каждый packet нет.
- Accounting не смешивается: decoded-media input, реально produced output, actual processor-pending output, static input latency и static output latency — разные величины/оси. Static `output_latency` нельзя выдавать за actual pending после reset/flush.
- `PlayerAudioOutput::write_samples` возвращает typed `AudioOutputWriteReport`, а не неоднозначный scalar sample count. `AudioOutputInputFrameCount` относится к PCM до channel conversion; `AudioOutputStreamFrameCount` — к prepared/queued PCM после channel conversion и resampling. Полнота определяется только как `prepared_output_frames == queued_output_frames`; различие input/output counts законно. Malformed frame alignment возвращает `AudioOutputWriteError`, а настоящий partial output остаётся fatal в player-core для direct, tempo и tempo EOF paths.
- Signalsmith EOF lifecycle: сначала продвинуть processing time вызовом `process` с `input_latency` frames тишины, затем извлечь хвост через `flush` минимум на `output_latency`. Оба куска возвращаются одним учтённым результатом; после завершения actual pending равен нулю.
- Rate automation привязана к processing time. Если один input/EOF проход пересекает границу старого и нового DSP segment, adapter вызывает backend отдельными упорядоченными chunks, а report сохраняет ordered segment spans. Нельзя заменять это одним process-вызовом со средним ratio.
- `reset` очищает DSP/history/pending; prime-only finish также возвращает processor в чистое состояние. Waveform tests проверяют сохранность последних samples и переходы для 0.25x, 0.5x, 1x, 2x и 4x, а не только длину.
- Warmup при первом переходе с direct 1x в tempo path использует не больше 600 ms уже декодированного PCM. Priming не должен повторно отправлять этот PCM на output.
- Output policy 2A: `AudioOutputWriteIntent::DirectDecodedPcm` — чистый 1.0x путь без limiter/soft-clip; при совпадающем формате он bit-transparent. `TempoProcessed` явно применяет protection к DSP output. Если processor уже активен и rate вернулся к 1.0x, его хвост сохраняется как `TempoProcessed`; direct path возвращается после lifecycle reset/нового media.
- `AudioOutputClockTiming` — нейтральный snapshot audible output-clock position и submitted output end (ring tail плюс PCM, уже отданный callback, но ещё не дошедший до DAC). CPAL-типы не выходят из `crates/audio`.
- Pause boundary: `PlayerAudioOutput::pause_and_freeze_clock` атомарно сериализуется с callback consumer, возвращает `AudioOutputClockTiming` и замораживает clock. Resume компенсирует wall pause duration. Если устройство не поддерживает physical pause, concrete output использует logical silence gate; настоящая ошибка pause не маскируется.
- EOF считается завершённым только после submitted DAC tail, а не только после опустевшего ring buffer.
- Frame alignment остаётся обязательным: interleaved ring producer/callback работают только целыми frames; split frame может навсегда поменять каналы местами.
- Allocation bounds: neutral tempo boundary и concrete channel mixer переиспользуют caller/output-owned buffers. Matching-rate channel conversion больше не создаёт обязательный packet-local `Vec`; существующий linear resampler при отличающемся device rate пока возвращает новый `Vec` и остаётся отдельной future optimization.
- Multichannel boundary: `AudioChannelLayout`/`AudioChannelPosition` в `audio-core` выражают neutral canonical speaker positions; `AudioOutputSpec` хранит sample rate + layout, а channel count вычисляется. `AudioDecoder::decoded_output_spec` атомарно связывает непустой PCM buffer с его format; player/output не получают Symphonia или CPAL types.
- Concrete `ChannelMixer` строит матрицу один раз при создании output. Same-count PCM копируется bit-for-bit; mono дублируется; positional 5.1 rear/side и 7.1-family сводятся в stereo с FL/FR=0 dB до нормализации, center/surround=−3 dB, LFE=0. Каждый stereo row статически нормализован по сумме модулей до full scale; dynamic limiter direct PCM не включается. `Discrete(n)` при изменении count и unsupported height layout дают typed `UnsupportedChannelConversion`, а не guessed downmix.
- Локальный `symphonia-codec-aac` patch обязан преобразовывать AAC coded element order в canonical Symphonia planes. Для indexed `channel_configuration` spatial role задаётся coded position/type, а не конкретным `element_instance_tag`; произвольные уникальные 4-bit tags совместимы. Для AAC 5.1 coded `FC,FL,FR,RL,RR,LFE` отображается в plane indices `[2,0,1,4,5,3]`, после чего interleaved PCM имеет `FL,FR,FC,LFE,RL,RR`. Config 3–7, arbitrary tags и duplicate rejection закреплены patch tests.

## Проверки

- `cargo test -p audio-core`
- `cargo test -p audio-signalsmith`
- `cargo test -p audio`
- `cargo test -p 'path+file://<REPO_ROOT>/crates/symphonia-codec-aac-patch#symphonia-codec-aac@0.6.0'`
- `cargo test -p audio-timestretch`
- Полный release gate дополнительно описан в `mem:task_completion` и `mem:player-core/playback-rate-contract-s32`.


## Session 27C decomposition (2026-07-11)

- `crates/audio/src/decoder.rs` теперь владеет только production factory и lifecycle Symphonia/Opus decoder-ов; neutral↔Symphonia codec/channel metadata mapping, packet adaptation и decoded PCM interleaving находятся в crate-private `decoder/conversion.rs`.
- `crates/audio/src/output.rs` остаётся facade/runtime owner-ом `AudioOutput`, CPAL stream и ring-buffer callback. Device capability/fallback selection находится в `output/configuration.rs`, pause/resume stream lifecycle — в `output/lifecycle.rs`, sample conversion + tempo output protection — в `output/processing.rs`, stateful packet-boundary linear resampling — в `output/resampler.rs`.
- Public API не изменился. Channel order/downmix, CPAL format/rate fallback, buffer targets, clock anchors/underrun accounting, direct/tempo protection policy, resampler carry/reset и tempo latency сохранены behavior-neutral.
- Полный census и два bounded follow-up prompt-а (`audio-core` facade и evaluation `audio-timestretch` adapter) находятся в `user/session_27c_audio_census_and_followups_2026-07-11.md`.


## S20 read-only audio decode capability (2026-07-21)

- `audio-core` теперь владеет neutral read-only boundary: `AudioDecodeCodecFamily`, typed `AudioDecodeCodecFamilyQuery`, distinct `AudioDecodeCapability::{Available, Unavailable}`, `AudioDecodeCapabilityQueryError::UnknownCodecFamily`, compact immutable `AudioDecodeCapabilitySnapshot` и `AudioDecodeCapabilityProvider`.
- Snapshot — `Copy` bitset без interior mutability; его query/iteration не создают decoder, не аллоцируют промежуточные collections и не смешивают unknown family с известной, но runtime-unavailable family.
- Family capability относится только к exact codec identities, уже прошедшим versioned static compatibility profile. Это не wildcard для любого будущего raw `adpcm*`/`pcm*`; future S21C обязан сначала выполнить static profile validation, затем intersect-ить runtime snapshot.
- Concrete `audio::ProductionAudioDecoderFactory` снимает snapshot через read-only Symphonia 0.6 `CodecRegistry::get_audio_decoder`; scanner boundary не содержит factory/create method. Opus декларируется через существующий concrete fallback, а не через fallible decoder construction.
- Current proven matrix: AAC, ADPCM (Symphonia registered MS/IMA WAV/IMA QT set), ALAC, FLAC, MP1/MP2/MP3, interleaved PCM registered set, Vorbis и Opus fallback. Mapped, но незарегистрированные G.722/G.726 и planar PCM не используются как runtime evidence.
- `app-egui::AppState` создаёт один production audio decoder factory, сохраняет его immutable capability snapshot отдельно от video `SystemCapabilities` и предоставляет app-owned accessor для будущего S21C selection; `player-core`/services/web-media-core не получают Symphonia types.
- Focused coverage: production registry parity, empty registry + disabled Opus fallback, neutral fake provider, structural read-only scan без decoder construction/state mutation и typed unknown-family rejection.
- Проверки: `cargo test -p audio-core -p audio`, `cargo check --workspace`, `cargo test -p app-egui --no-run`, strict Clippy для `audio-core`/`audio`, strict rustdoc, fmt, diff check, refactor guardrails и Serena diagnostics PASS. Workspace/app all-target strict Clippy остаётся blocked двумя pre-existing `app-egui` `large_enum_variant` diagnostics в `state/strong_media_open{,/pending}.rs`.


## S21C consumer (2026-07-21)
- `web-media-playback-plan` принимает immutable S20 `AudioDecodeCapabilitySnapshot` как часть общего playback capability snapshot и возвращает typed audio-layer rejection; decoder при планировании не создаётся.
- Детали: `mem:media-services/web-playback-planner-s21c-2026-07-21`.

## S28C audio-container proof consumer (2026-07-22)

- Parameterized planner proof закрепляет exact S20 intersection для current Ogg/Opus, CAF/WAVE/AIFF PCM, native FLAC и отдельных MP1/MP2/MP3 rows. Available exact family проходит до audio-only plan, отсутствующая family даёт typed `AudioUnavailable` до I/O и decoder construction.
- S20 API и production registry matrix не менялись. Container/transport/packet proof и CAF limitation: `mem:symphonia-demux/audio-containers-s28c-2026-07-22`.


## S30 exact SWF ADPCM fallback (2026-07-23)

- Конкретный `audio` crate (не нейтральный `audio-core`) владеет встроенным декодером Flash/SWF ADPCM для точного codec identity `A_ADPCM_SWF`.
- `create_audio_decoder()` маршрутизирует этот identity в project fallback до Symphonia; похожие строки и другие ADPCM dialects не подменяются.
- Поддерживаются mono/stereo и 2/3/4/5-bit коды. Полный block содержит 4096 frames, но final block packet-а может быть partial: после channel headers принимаются только целые interleaved channel code groups и нулевой byte-alignment tail. Delta считается нормативным bitwise accumulation отдельных shifted-step contributions (не сворачивается в одно умножение); reference — FFmpeg `libavcodec/adpcm.c`. Между packet-ами скрытого predictor/index state нет; `reset()` детерминированно no-op.
- Декодер строго проверяет header/index/alignment/truncation и возвращает typed `SwfAdpcmDecodeError`; при ошибке partial PCM наружу не выдаётся.
- Семейство ADPCM считается доступным только вместе: существующий Symphonia scope плюс exact SWF fallback. Neutral capability/media contracts не расширялись.
- Focused tests находятся в `crates/audio/src/decoder/swf_adpcm.rs`; factory/capability contract tests — рядом с соответствующими владельцами.
- Связанный codec foundation: `mem:codec-core/vp-flv-foundation-s30`.


## Opus multistream fallback и neutral channel order (2026-08-10)

- Symphonia 0.6 распознаёт Opus, но production decode по-прежнему проходит через project-owned crate-private adapter `audio::decoder::opus`. Runtime decoder выбирает single-stream `opus::Decoder` либо `opus::MSDecoder` только после структурной проверки полного `OpusHead`.
- `OpusHead` является authoritative metadata для channel count, mapping family/table и output gain. Family 0 поддерживает mono/stereo; family 1 поддерживает 1–8 каналов через multistream. Multichannel без полного header отклоняется typed error-ом, без угадывания mapping. Family 255/reserved намеренно не принимаются: у neutral boundary нет надёжной speaker semantics для arbitrary/discrete mapping.
- Family-1 Vorbis lane order переставляется при создании decoder-а в canonical `AudioChannelLayout`, прежде чем PCM пересечёт audio boundary. Для 5.1 canonical порядок: FL, FR, FC, LFE, RL, RR. `ChannelMixer` и output никогда не должны знать codec-specific lane order.
- Decode buffer рассчитывается как максимум 120 ms * channel count, PCM остаётся interleaved f32. Обязательный Opus output gain применяется до возврата `DecodedAudioFrame`.
- Playback clock Opus всегда 48 kHz. Поле input sample rate из `OpusHead` — только metadata и может быть нулём; `symphonia-demux::track_mapper` публикует 48 kHz даже при исходном metadata rate 44.1 kHz.
- Functional proof: реальный пакет, созданный libopus `MSEncoder`, проходит public `create_audio_decoder` -> multistream decode -> production `ChannelMixer` 5.1→stereo; отдельные parser tests закрепляют exact Wikimedia row-05 header/mapping. Реальный WebM fixture проверяется `symphonia-demux/tests/audio_fixture_decode_seek.rs`.
- Source `pre_skip`/seek preroll не спрятаны в decoder `reset()`: существующий lifecycle не различает initial start и post-seek discontinuity. Будущая sample-exact работа должна передать trim/discontinuity intent на demux/player boundary, а не менять ownership внутри codec adapter-а.
