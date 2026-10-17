# Фикстуры E-AC-3 `ac3-decoder`

Маленькие сырые потоки E-AC-3 (`.eac3`, 6 кадров по 6 блоков = 0,192 с при
48 кГц; у `5p1-32k-1000k` — 12 кадров по 3 блока), сгенерированные кодером
ffmpeg из синтетического сигнала. Реальные медиа сюда не кладутся
(`mem:testing/media-fixtures`).

## Как получены

`generate.sh` (ffmpeg n9.0.2, кодер `eac3`; повторный запуск даёт те же
байты). Сигнал — как у фикстур AC-3 (`../README.md`): на каждом канале свой
тон 0,4 плюс детерминированный шум ±0,015.

| роль | L | R | C | LFE | Ls / моно-surround | Rs |
|------|---|---|---|-----|--------------------|----|
| Гц   | 440 | 1000 | 2000 | 60 | 3000 | 7000 |

Исключения: `mono-48k-64k` — C = 2000 Гц; `stereo-48k-128k-cpl-high` —
L = 5000 Гц, R = 11 000 Гц (оба выше начала coupling).

## Состав и ожидаемые значения

Значения сверены с опциями кодера и `ffprobe -show_streams` / `-show_frames`
(side data «downmix info»: `center_mix_level_db` = Lo/Ro, `*_ltrt_db` = Lt/Rt).

| файл | Гц | кбит/с | acmod | LFE | блоков | особые опции → поля BSI |
|------|----|--------|-------|-----|--------|-------------------------|
| `mono-48k-64k.eac3` | 48000 | 64 | 1/0 | – | 6 | dialnorm 27; infomdate: mixlevel 105 дБ SPL, roomtyp 2 |
| `stereo-48k-96k.eac3` | 48000 | 96 | 2/0 | – | 6 | infomdate: dsurmod 2, dheadphonmod 2; coupling |
| `stereo-44k1-192k.eac3` | 44100 | 192 | 2/0 | – | 6 | copyrightb 1, origbs 0; coupling |
| `stereo-32k-640k.eac3` | 32000 | 640 | 2/0 | – | 6 | без coupling, без метаданных |
| `stereo-48k-128k-cpl-high.eac3` | 48000 | 128 | 2/0 | – | 6 | `-channel_coupling 1 -cpl_start_band 1`, тоны выше начала coupling |
| `3f-48k-256k.eac3` | 48000 | 256 | 3/0 | – | 6 | mixmdate: dmixmod Lo/Ro, lorocmixlev 0,5 (ffprobe −6 дБ), ltrtcmixlev 0,595 (−4,5 дБ) |
| `2f1r-48k-192k.eac3` | 48000 | 192 | 2/1 | – | 6 | |
| `quad-44k1-256k.eac3` | 44100 | 256 | 2/2 | – | 6 | |
| `2p1-48k-192k.eac3` | 48000 | 192 | 2/0 | + | 6 | |
| `5p1-48k-384k.eac3` | 48000 | 384 | 3/2 | + | 6 | mixmdate: Lt/Rt, ltrt 0,841/0,5, loro 1,0/0,707 (ffprobe 0/−3 дБ); infomdate: dsurexmod 2, mixlevel 100, roomtyp 1, adconvtyp HDCD |
| `5p1-32k-1000k.eac3` | 32000 | 1000 | 3/2 | + | 3 | 1 Мбит/с не помещается в 6 блоков (> 2048 слов): `numblkscod = 2`, `convsync` |

Все кадры — независимый substream 0, `bsid = 16`, `expstre = 1`, без AHT, SPX,
ECPL, TPNP: кодер ffmpeg этих инструментов не выдаёт. Они проверяются
синтетическими кадрами (`src/tests/eac3/builder.rs`: зависимые substream-ы и
`chanmap`, другие программы, пониженные частоты, 1/2 блока, синтаксис AHT/SPX,
ошибки состава access unit-а) и dev-прогоном реальных потоков
(`real_eac3_streams_parse_with_exact_bit_accounting`, см. `mem:ac3-decoder/core`).

SHA-256 (первые 16 hex): `2f1r-48k-192k` 1f5f4edd0d4ea432, `2p1-48k-192k`
9dc089284221ced5, `3f-48k-256k` a520f2eb98f28e1e, `5p1-32k-1000k`
6f7d443f163d14dd, `5p1-48k-384k` b2205a5106dc2094, `mono-48k-64k`
14b41c4db73a6c6b, `quad-44k1-256k` da9cd7dbc1c58f73, `stereo-32k-640k`
46b1607548c0dcd0, `stereo-44k1-192k` 80252315d040a489,
`stereo-48k-128k-cpl-high` 85c42079a9881b2b, `stereo-48k-96k` 05cebc7d5ee4939e.

## Выдержки PCM ffmpeg (пакет D4)

`*.ffmpeg-f32le` — PCM декодера ffmpeg n9.0.2 (f32le, interleaved, порядок
каналов ffmpeg: FL FR FC LFE … SL SR) для отрезка `[from, to)` отсчётов на
канал (команда `reference` в `generate.sh`). Сверку ведёт
`src/tests/eac3/pcm_reference.rs` методом D2 (чистые бины — в пределах
сетки ffmpeg ≥ 90 дБ, dither — по энергии ±3 дБ).

| файл | отсчёты | что покрывает |
|------|---------|---------------|
| `stereo-48k-96k.ffmpeg-f32le` | 0–3072 | начало потока (нулевые хвосты, задержка 0), coupling, dither |
| `5p1-48k-384k.ffmpeg-f32le` | 3072–4608 | 3/2 + LFE |
| `5p1-32k-1000k.ffmpeg-f32le` | 3072–4608 | кадры по 3 блока (`numblkscod = 2`) при 32 кГц |

Зависимые substream-ы, TPNP, пониженные частоты и DRC зависимого
проверяются кадрами `src/tests/eac3/builder.rs`, сведёнными с этими
фикстурами (`src/tests/eac3/program.rs`, `program_synthesis.rs`): ffmpeg
таких потоков не кодирует, а пониженные частоты и не декодирует.
