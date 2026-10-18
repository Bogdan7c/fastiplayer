# Фикстуры «дубляжи yt-dlp»

Сгенерированы `generate.sh` из синтетических источников ffmpeg. Ничего не скачано,
лицензия — та же, что у репозитория. Тест
(`src/web_media_open/audio_languages/vertical_tests.rs`) опирается только на свойства ниже.

| Файл | Роль в JSON yt-dlp | Содержимое | Тон |
|---|---|---|---|
| `video-only.mp4` | video-only формат | H.264 baseline 64x64, 10 к/с, 2 с, без звука | — |
| `audio-original.mp3` | audio-only, язык `en-US`, `language_preference` 10 | MP3 моно 22.05 кГц, 2 с | 440 Гц |
| `audio-dub-a.mp3` | audio-only, язык `ru`, `language_preference` -1 | то же | 880 Гц |
| `audio-dub-b.mp3` | audio-only, язык `de`, `language_preference` -1 | то же | 1320 Гц |
