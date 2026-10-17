# Фикстуры HLS «несколько версий звука»

Сгенерированы `generate.sh` из синтетических источников ffmpeg (`testsrc2` 64×64
10 к/с, синус; 0.5 с). Ничего не скачано, лицензия — та же, что у репозитория.
Байты зависят от версии ffmpeg, поэтому тест
(`src/native_startup/hls/audio_versions_vertical_tests.rs`) опирается только на
свойства ниже (`ffprobe -show_entries stream=codec_name,profile,sample_rate,channels`).

| Файл | Роль в HLS master | Кодек | Частота | Каналы | Тон |
|---|---|---|---|---|---|
| `video-only-h264.ts` | video variant без звука (`VideoOnly` layout) | H.264 Constrained Baseline 64×64 | — | — | — |
| `audio-aac-2ch.ts` | rendition group `aac` | AAC-LC | 48000 | 2 | 440 Гц |
| `audio-eac3-5p1.ts` | rendition group `ec3` (ATSC `stream_type 0x87`) | E-AC-3 | 48000 | 6 (5.1 side) | 880 Гц |

Master в тесте собран по Apple authoring spec: отдельная audio group на кодек и
`EXT-X-STREAM-INF` на каждую group с одним и тем же видео — именно такой layout
требует выбора «эквивалентного» video row при смене версии звука.
