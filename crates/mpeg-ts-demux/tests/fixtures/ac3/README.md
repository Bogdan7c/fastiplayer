# Фикстуры AC-3 / E-AC-3 в MPEG-TS

Сгенерированы `generate.sh` из синтетических источников ffmpeg (`testsrc2` 64×64
10 к/с и синус 440 Гц, 0.5 с). Ничего не скачано, лицензия — та же, что у
репозитория. Байты зависят от версии ffmpeg, поэтому тесты
(`../../ac3_transport_stream.rs`) опираются только на свойства ниже; эталон —
`ffprobe -show_entries stream=codec_name,sample_rate,channels:stream_tags=language`
и `ffprobe -select_streams a -show_entries packet=pts,size`.

| Файл | Сигнализация в PMT | Кодек | Частота | Каналы | Язык | Аудио пакетов | Размер кадра | Первый PTS |
|---|---|---|---|---|---|---|---|---|
| `h264-ac3-stereo-48k.ts` | ATSC `stream_type 0x81` + registration `AC-3`; + H.264 | AC-3 | 48000 | 2 | — | 16 | 384 | 126000 |
| `h264-eac3-5p1-44k1.ts` | ATSC `stream_type 0x87` + registration `EAC3`; + H.264 | E-AC-3 | 44100 | 6 | — | 15 | 834/836 | 126000 |
| `audio-only-ac3-dvb-mono.ts` | DVB `0x06` + registration `AC-3` + `AC-3_descriptor 0x6a` + `ISO_639 0x0a` | AC-3 | 48000 | 1 | rus | 16 | 256 | 126000 |
| `audio-only-eac3-dvb-stereo-32k.ts` | DVB `0x06` + registration `EAC3` + `enhanced_AC-3_descriptor 0x7a` + `ISO_639 0x0a` | E-AC-3 | 32000 | 2 | eng | 11 | 576 | 126000 |
| `ac3-broadcast-2p0-5p1-2p0-48k.ts` | ATSC `0x81` + registration `AC-3`, audio-only | AC-3 | 48000 | 2 → 6 → 2 (по 16 кадров) | — | 48 | 768 / 1792 / 768 | 126000 |

ffmpeg кладёт в один PES только целые кадры; разрезанные границами PES syncframe-ы,
DVB-дескриптор без registration, dependent substream-ы, дополнительные программы
E-AC-3 и повреждения проверяются вручную собранными потоками в
`src/tests/ac3_elementary.rs`.

`ac3-broadcast-2p0-5p1-2p0-48k.ts` — склейка `crates/audio/tests/fixtures/ac3/tones-{stereo,5.1,stereo}.ac3`
(`-c:a copy`), то есть вещательная смена 2.0 → 5.1 → 2.0 на одном PID-е. ffprobe показывает формат
первого кадра (2 канала); PTS идут без разрыва с шагом 2880. Тесты: `../../ac3_broadcast_format_switch.rs`
(нет `TracksChanged`) и `media-source-open` `local_media/ac3_broadcast_switch_tests.rs` (декод по участкам).
