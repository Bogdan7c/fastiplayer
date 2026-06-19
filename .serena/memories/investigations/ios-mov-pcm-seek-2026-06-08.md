# iOS MOV PCM Seek Investigation (2026-06-08)

Этот memory является сохранённым контекстом расследования; отдельного проектного markdown-файла больше нет.

Проблема: `<MEDIA_DIR>/ios-hevc-pcm.mov` (QuickTime/iOS MOV, HEVC + `pcm_s16le`/`lpcm`, 48 kHz stereo) после seek даёт сильные лаги звука и видео выглядит как замедленное. Остальные форматы и iOS MOV AAC работают нормально.

Ключевые факты:
- audio time base `1/48000`, video time base `1/600`;
- PCM packet обычно `duration=1024`, `size=4096`, то есть `21.333 ms`;
- output device выбрал `44100 Hz`, ресемплинг `48000 -> 44100` включён;
- `AudioOutput::new` создаёт `AudioClock` на `stream_rate`, значит простая ошибка clock `48000` vs `44100` почти исключена;
- `start_seek_transaction` уже делает pause, generation bump, clear pending packets, reset decoder/clocks, clear output buffer/resampler/clock.

Основные гипотезы:
1. Вероятнее всего MOV/PCM packet timing после video-based seek приходит неверным через `symphonia-demux` / локальный `symphonia-format-isomp4-patch`: проверить первые 20 audio packets после seek (`pts`, `duration`, raw `track_pts`, raw `track_duration`, `data.len()`, drop/queue decision).
2. Возможен audio underrun после seek commit: `AudioClock` не двигается на silence, поэтому video scheduler идёт за медленным audio clock. Проверить `audio_buffer_level_ms`, underrun counters, written/played samples.
3. Менее вероятно: `pause -> clear -> play` race или first CPAL playback anchor после resume.

Следующий шаг: не править scheduler tuning вслепую; сначала добавить временную диагностику/probe. Для S16LE stereo invariant: `packet.data.len() / 4 == track_duration.units`, `duration_seconds = duration_units / 48000`. Если invariant ломается после seek, смотреть `crates/symphonia-format-isomp4-patch/src/stream.rs`; если invariant нормальный, но buffer падает и underruns растут, смотреть `audio_runtime` / `AudioOutput` / worker wakeup scheduling.

Update 2026-06-09: гипотеза 1 исправлена в `crates/symphonia-format-isomp4-patch`. Root cause подтвердился probe-ом: до фикса QuickTime LPCM после seek отдавал `track_duration=1`, `data.len=4` на каждый PCM frame. `IsoMp4Reader` теперь coalesce-ит PCM/LPCM на boundary `IsoMp4Reader -> Packet` только когда текущий MP4 sample сам длится один audio frame (`sample_duration == 1`). Если sample entry сообщает `max_frames_per_packet > 1`, это значение используется как верхняя граница, иначе reader chunk-ит one-frame samples в `1024`-frame packets; размер также bounded этим reader chunk size. `pts/dts` остаются от первого frame-sample, `dur` суммирует grouped samples, `data` читается одним contiguous span, `TrackState.next_sample`/`next_sample_pos` продвигаются на фактическое число samples. Coalescing ограничен текущим segment/chunk и не применяется к AAC/ALAC/Opus/non-PCM или к PCM samples, которые уже имеют длительность больше одного frame. Повторный probe на `<MEDIA_DIR>/ios-hevc-pcm.mov` после seek к 60s показал first tail packet `384` frames / `1536` bytes, затем стабильные `1024` frames / `4096` bytes. Focused tests добавлены в `demuxer.rs` для reader chunk guard, already-packetized PCM guard, 1024-frame packet, tail 736 frames, chunk boundary и non-PCM guard; `stsd.rs` проверяет, что QuickTime `lpcm` v2 сохраняет `frames_per_packet=1024` в codec params.
