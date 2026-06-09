# 15. iOS MOV PCM Seek Investigation

Дата: 2026-06-08.

Статус: расследование без правок кода. Документ сохраняет контекст, чтобы
продолжить работу на следующей сессии.

## Симптом

На тестовом ассете `<MEDIA_DIR>/ios-hevc-pcm.mov` после seek звук
начинает сильно лагать, а видео выглядит как замедленное воспроизведение.

Что известно по сравнению с другими файлами:

- остальные форматы воспроизводятся нормально;
- iOS/MOV с AAC тоже работает нормально;
- проблема проявляется именно на iOS/QuickTime MOV с PCM audio;
- до seek файл стартует и играет штатно.

Фрагмент лога важный для контекста:

```text
Audio track выбран; decoder/output будут созданы лениво track_id=2 codec_id=A_PCM_S16LE sample_rate=Some(48000) channels=Some(2)
Audio output device device=default stream_rate=44100 stream_channels=2 format=F32
Будет использоваться ресемплинг decoder_rate=48000 stream_rate=44100
AudioOutput создан buffer_capacity=176400
Audio stream paused
No reference found for poc ...
Audio stream started
```

## Факты По Ассету

`ffprobe` по `<MEDIA_DIR>/ios-hevc-pcm.mov`:

- container: QuickTime/MOV (`qt`);
- video: HEVC Main, `hvc1`, 1920x1080, около 30 fps;
- video time base: `1/600`;
- audio: `pcm_s16le`, tag `lpcm`;
- audio sample rate: `48000`;
- audio channels: `2`;
- audio time base: `1/48000`;
- audio packet обычно `duration=1024`, то есть `21.333 ms`;
- audio packet обычно `size=4096`, то есть `1024 frames * 2 channels * 2 bytes`;
- встречаются shorter packets, например `duration=736`;
- первый audio packet имеет `pts=-68` и side data `skip_samples=68`, но это
  всего около `1.4 ms` и само по себе не похоже на причину сильного лага.

Пример нормальных audio packets около 60 секунд:

```text
pts=2878220 pts_time=59.962917 duration=1024 duration_time=0.021333 size=4096
pts=2879244 pts_time=59.984250 duration=1024 duration_time=0.021333 size=4096
pts=2880268 pts_time=60.005583 duration=1024 duration_time=0.021333 size=4096
```

Видео keyframe spacing по `ffprobe` примерно `0.93 s`, поэтому проблема не
выглядит как обычная цена длинного GOP после H.265 seek.

## Что Уже Проверено

### Seek Lifecycle В Player Core

В `PlayerSession::start_seek_transaction` явный lifecycle выглядит корректно:

- audio output ставится на паузу;
- video decoder flush выполняется до нового seek generation;
- `seek_generation` увеличивается;
- pending audio/video packets очищаются;
- video decoder state сбрасывается;
- clocks перепривязываются к seek target/runtime base;
- audio decoder reset вызывается, если decoder уже создан;
- audio output buffer очищается по generation;
- output clear подтверждается через `audio_buffer_clear_ack`.

Важные символы:

- `crates/player-core/src/session/seek_transaction.rs`
  - `PlayerSession::start_seek_transaction`
  - `PlayerSession::pause_audio_output_for_seek`
  - `PlayerSession::resume_audio_output_after_seek`
  - `PlayerSession::should_drop_demuxed_audio_packet_for_seek`
  - `PlayerSession::finish_seek_commit_if_ready`
- `crates/player-core/src/pipeline.rs`
  - `PlaybackPipeline::clear_pending_packets_for_seek`
  - `PlaybackPipeline::reset_audio_decoder`
  - `PlaybackPipeline::clear_audio_output_for_seek`
  - `PlaybackPipeline::reset_clocks_for_seek`

Вывод: гипотеза "после seek забыли очистить audio decoder/output/clock" слабая.
Базовый reset path в коде есть.

### Audio Output И Resampling

`AudioOutput::new` создаёт `AudioClock` с `stream_rate`, а не с
`decoder_rate`.

Для этого ассета:

- decoder rate: `48000`;
- CPAL stream rate: `44100`;
- clock rate: `44100`;
- `write_samples` сначала конвертирует layout, потом ресемплит, потом пишет в
  ring buffer;
- `record_written` получает количество samples уже после ресемплинга;
- CPAL callback продвигает clock только по реально заполненным samples, silence
  не двигает media clock.

Важные символы:

- `crates/audio/src/output.rs`
  - `AudioOutput::new`
  - `AudioOutput::write_samples`
  - `AudioOutput::clear_buffer_for_seek`
  - `AudioOutput::fill_buffer`
  - `LinearResampler::resample_interleaved`
- `crates/audio/src/clock.rs`
  - `AudioClock::new`
  - `AudioClock::now`
  - `AudioClock::record_written`
  - `AudioClock::record_output_callback`
  - `AudioClock::reset`

Вывод: простая ошибка "clock считает 44.1 kHz samples как 48 kHz" почти
исключена. Clock и buffer level считаются в output stream units.

### Demux -> Audio Runtime Path

Цепочка audio packet-а:

```text
SymphoniaDemuxer
  -> packet_mapper::convert_packet
  -> demux_admission::route_demuxed_packet
  -> PendingAudioPacket::with_timing
  -> PlayerSession::process_pending_audio_packets_with_buffer_limit
  -> PlayerSession::process_audio_packet_with_timing
  -> SymphoniaAudioDecoder::decode
  -> AudioOutput::write_samples
  -> CPAL callback / AudioClock
```

Важные символы:

- `crates/symphonia-demux/src/packet_mapper.rs`
  - `convert_packet`
  - `packet_duration`
  - `packet_track_duration`
  - `packet_timestamp_to_duration`
- `crates/player-core/src/session/tick/demux_admission.rs`
  - `route_demuxed_packet`
  - `audio_packet_timing_from_media_packet`
  - `audio_packet_duration_units`
  - `read_demux_packets`
- `crates/player-core/src/session/audio_runtime.rs`
  - `process_audio_packet_with_timing`
  - `trim_decoded_audio_to_clock_base`
  - `classify_seek_audio_gate`
- `crates/audio/src/decoder.rs`
  - `SymphoniaAudioDecoder::decode`
  - `symphonia_packet_ref_from_encoded_packet`

Audio preroll policy:

- complete audio packet before target drops in
  `should_drop_demuxed_audio_packet_for_seek`;
- partially crossing packet is decoded and trimmed in
  `trim_decoded_audio_to_clock_base`;
- drop complete pre-target packet requires `packet.duration`;
- if duration is missing/wrong after MOV seek, packet may be queued instead of
  dropped.

### Context7 / Symphonia Contract

Context7 по Symphonia подтвердил:

- `SeekMode::Accurate` должен seek-аться к позиции не позже target;
- `SeekedTo` содержит `track_id`, `required_ts`, `actual_ts`;
- packet duration в Symphonia 0.6 отдельный field и должен использоваться как
  duration packet-а;
- после seek caller должен сам решать, какие pre-target packets отбросить.

## Подозрительные Места

### 1. MOV/PCM Packet Timing После Seek

Самая вероятная зона: `symphonia-demux` / локальный
`symphonia-format-isomp4-patch`.

Почему:

- проблема только на MOV/PCM;
- MOV/AAC работает;
- другие контейнеры работают;
- player-core reset path выглядит нормальным;
- PCM packet timing очень плотный и зависит от точного `duration`;
- `should_drop_demuxed_audio_packet_for_seek` зависит от корректного
  `packet.duration`;
- `trim_decoded_audio_to_clock_base` зависит от корректного normalized
  `packet_pts`.

Проверить:

- залогировать первые 20 audio packets после seek;
- сравнить `packet.pts`, `packet.duration`, `track_pts.units`,
  `track_duration.units`, `data.len()`;
- для S16LE stereo invariant должен быть:
  `packet.data.len() / 4 == track_duration.units`.

### 2. Audio Underrun После Commit

Если после seek audio buffer быстро уходит в underrun, `AudioClock` почти не
движется, потому что silence не двигает media time. Тогда видео scheduler будет
идти за медленным audio clock, и визуально получится замедленное видео.

Проверить:

- `audio_buffer_level_ms` сразу перед commit и после `Audio stream started`;
- `underrun_callbacks`;
- сколько samples записано в output после seek;
- сколько callbacks CPAL заполнились silence.

### 3. Fast Preroll Accounting На Dense PCM

В `run_seek_fast_preroll_catch_up` есть специальный режим для active Accurate
seek. Для dense audio interleave complete dropped audio preroll не расходует
demux budget при active deadline.

Тесты уже покрывают часть этого сценария:

- `active_accurate_seek_demux_budget_ignores_dropped_audio_preroll`;
- `active_accurate_seek_interleaves_demux_and_decoder_io_during_fast_preroll`.

Но всё ещё стоит проверить реальный MOV/PCM:

- сколько `dropped_seek_audio_preroll_packets` за seek;
- доходит ли fast-preroll до target video packet за один bounded pass;
- есть ли pass, где весь work состоит только из dropped audio packets и helper
  считает это отсутствием progress.

### 4. Pause/Clear/Play Race

`AudioOutput::clear_buffer_for_seek` чистит ring buffer, resampler и clock, но
не может отменить samples, уже переданные backend-у CPAL до pause/clear.

Вероятность ниже: это скорее короткий хвост, а не постоянное замедление. Но
стоит проверить first callback после resume и playback anchors.

## Текущие Гипотезы

| Вероятность | Гипотеза | Как проверить |
| --- | --- | --- |
| 45% | После video-based seek MOV/PCM packets приходят с неправильным `pts`/`duration` в Symphonia/isomp4 path. | Лог первых 20 audio packets после seek: target, pts, duration, raw track pts/duration, data len, drop/queue decision. |
| 30% | После seek audio output underrun-ит, clock почти не движется, и video scheduler синхронизируется по медленному audio clock. | Лог `audio_buffer_level_ms`, underruns, written/played samples после commit/resume. |
| 15% | Race вокруг `pause -> clear -> play` оставляет stale backend tail или кривой first playback anchor. | Лог clear generation ack, first callback after play, filled/silence samples, `audio_clock_now`. |
| 10% | Локальный `symphonia-format-isomp4-patch` не полностью совместим с QuickTime `lpcm` sample tables после seek. | Probe без UI: после seek сравнить `duration`, `data.len()`, decoded sample count; для S16LE stereo `data.len()/4 == duration_units`. |

## Что Сделать Следующим Шагом

Не начинать с архитектурной правки. Сначала добавить временную диагностику или
маленький probe, чтобы отличить timing bug от underrun.

Минимальный debug-log после seek:

- target position;
- seek generation;
- для первых 20 audio packets:
  - normalized `packet.pts`;
  - normalized `packet.duration`;
  - raw `track_pts.units`;
  - raw `track_duration.units`;
  - `packet.data.len()`;
  - result: dropped as seek preroll / queued / decoded / trimmed empty /
    written samples;
- audio gate status;
- `audio_buffer_level_ms`;
- underrun counters;
- first CPAL callback after resume: filled samples, silence samples;
- current `audio_clock_now` и `media_position_from_audio_clock`.

Ожидаемые invariants для хорошего PCM packet-а:

```text
channels = 2
bytes_per_sample = 2
bytes_per_frame = 4
duration_units = data.len / 4
duration_seconds = duration_units / 48000
```

Для обычного packet-а:

```text
data.len = 4096
duration_units = 1024
duration_seconds = 0.021333...
```

Если invariant ломается после seek, идти в
`crates/symphonia-format-isomp4-patch/src/stream.rs` и sample-table seek path.

Если invariant нормальный, но buffer после commit падает к нулю и растут
underruns, идти в `audio_runtime` / `AudioOutput` / worker wakeup scheduling.

## Команды, Которые Уже Использовались

Общий stream info:

```bash
ffprobe -hide_banner -show_streams -show_format <MEDIA_DIR>/ios-hevc-pcm.mov
```

Audio packets около начала:

```bash
ffprobe -hide_banner \
  -select_streams a:0 \
  -show_packets \
  -show_entries packet=pts,pts_time,dts,dts_time,duration,duration_time,size,pos,flags \
  -read_intervals 0%+0.12 \
  -of compact=p=1:nk=0 \
  <MEDIA_DIR>/ios-hevc-pcm.mov
```

Audio packets около 60 секунд:

```bash
ffprobe -hide_banner \
  -select_streams a:0 \
  -show_packets \
  -show_entries packet=pts,pts_time,dts,dts_time,duration,duration_time,size,pos,flags \
  -read_intervals 60%+0.12 \
  -of compact=p=1:nk=0 \
  <MEDIA_DIR>/ios-hevc-pcm.mov
```

Video packets/keyframes около seek target:

```bash
ffprobe -hide_banner \
  -select_streams v:0 \
  -show_packets \
  -show_entries packet=pts,pts_time,dts,dts_time,duration,duration_time,size,pos,flags \
  -read_intervals 60%+0.25 \
  -of compact=p=1:nk=0 \
  <MEDIA_DIR>/ios-hevc-pcm.mov
```

## Правила На Завтра

- Не чинить symptom tuning-ом scheduler-а, пока не проверены packet timing и
  underrun.
- Не менять boundary/API без отдельного решения.
- Если понадобится новый boundary method, сначала описать владельца состояния,
  инвариант и focused tests.
- Если выяснится, что причина в `symphonia-format-isomp4-patch`, добавить
  regression/probe на MOV PCM seek, а не править только этот один ассет.
