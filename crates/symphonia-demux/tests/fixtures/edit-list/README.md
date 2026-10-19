# Фикстура MP4 со списками правок (`elst`)

`audio-delayed-bframes.mp4` создана `generate.sh` (ffmpeg, libx264 + нативный `aac`) из
синтетических источников: `testsrc` 32x32 24 к/с и синус 440 Гц, длительность 0.5 с,
около 4 КБ. Ничего не скачано, лицензия — та же, что у репозитория. Байты зависят от версии
ffmpeg; тест опирается только на свойства ниже (они проверены дампом боксов и `ffprobe`).

- Видео: H.264 Main с B-frame, `elst = [(24000, media_time 1024)]` при media timescale 12288 —
  первый показываемый кадр имеет media CTS 1024 (`ffprobe`: `start_time = 0.000000`).
- Аудио: AAC, `elst = [(10976, -1), (25024, 0)]` — ведущая пустая правка 10976 тиков при
  `mvhd`/`mdhd` timescale 48000 (`ffprobe`: `start_time = 0.228667`), `stts`/`tfdt` с 0.

Потребитель: `../../edit_list_av_sync.rs` — файл через production `SymphoniaDemuxer`
проверяется на разницу старта A/V, равную `ffprobe start_time`.
