# Фикстуры встроенных субтитров

Крошечные (единицы килобайт) файлы для `crates/symphonia-demux/tests/subtitle_tracks.rs`.
Их можно пересоздать скриптом `generate.sh` (нужен `ffmpeg`, проверено на n9.0.2):

```sh
crates/symphonia-demux/tests/fixtures/subtitles/generate.sh
```

Скрипт содержит точные команды ffmpeg. Видео — `lavfi testsrc` 64x64, 10 fps, 3 с; звук — синус 440 Гц.
Файлы не детерминированы побитово (ffmpeg пишет версию и UID), поэтому тесты проверяют только
известные ниже тайминги, тексты, языки, названия и флаги.

| Файл | Содержимое |
|---|---|
| `two-srt-tracks.mkv` | VP8 (язык `eng`, название «Main video»), Opus mono (`jpn`, «Japanese mono»), SRT `eng` «English» (default), SRT `rus` «Russian SDH» (forced + hearing impaired) |
| `ass-with-font.mkv` | VP8 и одна ASS-дорожка `eng` «Styled»; вложение `NotoSansHebrew-Regular.ttf` (`font/ttf`) |
| `two-audio-tracks.mkv` | VP8 (`eng`, «Main video»), FLAC 22050 Гц стерео — синус 440 Гц (`rus`, «Dub», default), Opus 48000 Гц моно — синус 880 Гц (`eng`, «Original»); 3 с, ~42 КБ. Для `audio_track_switch` в `player-core` и `audio_tracks.rs` |
| `two-audio-two-srt.mkv` | «Фильм с двумя озвучками и субтитрами» для памяти выбора: VP8, те же две аудиодорожки, что в `two-audio-tracks.mkv` (FLAC `rus` «Dub» default, Opus `eng` «Original»), и те же две SRT, что в `two-srt-tracks.mkv` (`eng` «English» default, `rus` «Russian SDH»); ~43 КБ |
| `aac-pce-5p1.mkv` | Только AAC-LC 48 кГц 5.1(side) с Program Config Element (`channel_configuration = 0`): синус 880 Гц в канале FC, 1 с, ~8 КБ. Для `aac_pce_playback.rs` |
| `low-fps-vp8-opus.mkv` | Низкий fps со звуком, достаточно длинный для обычного (не короткого) старта: VP8 64x64 10 fps + Opus mono 48 кГц (синус 440 Гц), 8 с, ~34 КБ. Для `startup_decode_ahead.rs` в `player-core` |
| `mov-text-rus.mp4` | H.264 и дорожка `mov_text` с языком `rus` (из `mdhd`) |

Реплики SRT: «Hello» / «Привет» на 0.5–1.5 с и «Second line» / «Второй» на 2.0–2.8 с.
ASS-реплика: `Styled {\b1}line{\b0}` на 0.5–1.5 с.

## Лицензии

- Видео и звук сгенерированы ffmpeg-источниками `testsrc` и `sine`, авторских прав не несут.
- Звук `two-audio-tracks.mkv` — тоже синусы ffmpeg (`sine`), сгенерированы скриптом; авторских прав нет.
- Тексты реплик написаны для тестов.
- Вложенный шрифт — `crates/subtitle-raster/fonts/NotoSansHebrew-Regular.ttf` (Noto Project Authors,
  SIL Open Font License 1.1, текст лицензии: `crates/subtitle-raster/fonts/OFL.txt`).
