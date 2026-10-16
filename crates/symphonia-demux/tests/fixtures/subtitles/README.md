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
| `mov-text-rus.mp4` | H.264 и дорожка `mov_text` с языком `rus` (из `mdhd`) |

Реплики SRT: «Hello» / «Привет» на 0.5–1.5 с и «Second line» / «Второй» на 2.0–2.8 с.
ASS-реплика: `Styled {\b1}line{\b0}` на 0.5–1.5 с.

## Лицензии

- Видео и звук сгенерированы ffmpeg-источниками `testsrc` и `sine`, авторских прав не несут.
- Тексты реплик написаны для тестов.
- Вложенный шрифт — `crates/subtitle-raster/fonts/NotoSansHebrew-Regular.ttf` (Noto Project Authors,
  SIL Open Font License 1.1, текст лицензии: `crates/subtitle-raster/fonts/OFL.txt`).
