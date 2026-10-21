# Фикстура HEVC-в-MP4 от x265 (`hvcC` больше 1 КиБ)

`x265-large-hvcc.mp4` создана `generate.sh` (ffmpeg + libx265) из синтетического `testsrc`
64x64 24 к/с, 6 кадров, около 6 КБ, без звука. Ничего не скачано, лицензия — та же, что
у репозитория. Байты зависят от версий ffmpeg/x265; тест опирается только на свойства ниже
(проверены дампом боксов и `ffprobe`).

- Sample entry `hvc1`, HEVC Main; `hvcC` ≈ 2,4 КиБ, потому что x265 кладёт туда prefix-SEI
  со строкой своих настроек. Прежний upstream-лимит isomp4 (1 КиБ) отвергал такой файл
  целиком: «isomp4 (hvcC): atom size is greater than 1 kb».
- `hvcC` содержит по одному VPS/SPS/PPS.
- 6 пакетов, ключевые кадры (`ffprobe` флаг `K`) на PTS 0 и 0.125 с (keyint 3).

Потребитель: `../../hevc_x265_mp4.rs` — файл через production `SymphoniaDemuxer`.
