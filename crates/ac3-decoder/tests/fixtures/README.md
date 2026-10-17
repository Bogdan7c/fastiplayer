# Фикстуры `ac3-decoder`

Маленькие сырые потоки AC-3 (`.ac3`, ровно 6 syncframe-ов = 0,192 с при
48 кГц), сгенерированные кодером ffmpeg из синтетического сигнала. Реальные
медиа сюда не кладутся (`mem:testing/media-fixtures`).

## Как получены

`generate.sh` (ffmpeg n9.0.2, кодер `ac3`). Сигнал — `aevalsrc`: на каждом
канале свой тон амплитуды 0,4 плюс детерминированный шум ±0,015. Частоты
по ролям каналов:

| роль | L | R | C | LFE | Ls / моно-surround (BC) | Rs |
|------|---|---|---|-----|-------------------------|----|
| Гц   | 440 | 1000 | 2000 | 60 | 3000 | 7000 |

Исключение: `stereo-48k-160k-cpl-high.ac3` — L = 5000 Гц, R = 11 000 Гц
(оба тона выше начала coupling, проверка восстановления каналов из coupling).

## Состав и ожидаемые значения

Значения сверены с настройками кодера и `ffprobe -show_streams` /
`ffprobe -show_frames` (side data «downmix info»: `center_mix_level_db`,
`surround_mix_level_db` — это уровни Lo/Ro; поля Lt/Rt и `preferred_downmix_type`
ffprobe для AC-3 не заполняет, поэтому для них источник истины — настройки кодера).

| файл | Гц | кбит/с | acmod | LFE | особые опции → поля BSI |
|------|----|--------|-------|-----|-------------------------|
| `mono-48k-96k.ac3` | 48000 | 96 | 1/0 | – | `-dialnorm -27` → dialnorm 27; `-audio_service_type co` → bsmod 5 |
| `stereo-48k-192k.ac3` | 48000 | 192 | 2/0 | – | `-dsur_mode on` → dsurmod 2; dialnorm 20; coupling с `cplbegf` 8 |
| `stereo-44k1-128k-plain.ac3` | 44100 | 128 | 2/0 | – | без coupling и rematrixing; `dsurmod` 1 |
| `stereo-48k-32k.ac3` | 48000 | 32 | 2/0 | – | минимальный битрейт: `cplbegf` 0, много мантисс без бит |
| `3f-48k-256k.ac3` | 48000 | 256 | 3/0 | – | `cmixlev` −6 дБ; mixlevel 105 дБ SPL, roomtyp 1 |
| `2f1r-32k-192k.ac3` | 32000 | 192 | 2/1 | – | `surmixlev` −3 дБ |
| `3f1r-48k-256k.ac3` | 48000 | 256 | 3/1 | – | bsmod 2 (VI) |
| `2f2r-48k-320k.ac3` | 48000 | 320 | 2/2 | – | copyrightb 1, origbs 0 |
| `3f2r-48k-448k-cpl.ac3` | 48000 | 448 | 3/2 | – | `-channel_coupling 1 -cpl_start_band 5` |
| `mono-lfe-48k-128k.ac3` | 48000 | 128 | 1/0 | + | |
| `stereo-lfe-44k1-192k.ac3` | 44100 | 192 | 2/0 | + | |
| `2f1r-lfe-48k-256k.ac3` | 48000 | 256 | 2/1 | + | |
| `3f-lfe-32k-256k.ac3` | 32000 | 256 | 3/0 | + | |
| `3f1r-lfe-48k-384k.ac3` | 48000 | 384 | 3/1 | + | |
| `5p1-48k-640k-annexd.ac3` | 48000 | 640 | 3/2 | + | Annex D (bsid 6): dmixmod Lt/Rt, ltrt 0,841 / 0,5, loro 1,0 / 0,707, dsurexmod 2, dheadphonmod 1, adconvtyp HDCD |
| `5p1-44k1-448k-annexd.ac3` | 44100 | 448 | 3/2 | + | Annex D: dmixmod Lo/Ro, уровни по умолчанию (0,595 / 0,5), dsurexmod 3 (PLIIz) |
| `5p1-32k-192k.ac3` | 32000 | 192 | 3/2 | + | bsmod 7 при 3/2 → karaoke; полоса кодера урезана до 73 бинов (тон Rs 7 кГц отрезан) |
| `stereo-48k-160k-cpl-high.ac3` | 48000 | 160 | 2/0 | – | `-channel_coupling 1 -cpl_start_band 1`, тоны выше начала coupling |

Не покрыто ffmpeg (проверяется синтетическими кадрами `src/tests/synthetic.rs`):
режим 1+1, `addbsi`, тайм-код, `compr`/`langcod`, bsid 9/10, нарушения §7.10.2.
Блочное переключение (`blksw`) (для синтеза — см. `stereo-48k-192k-short0` ниже), delta bit allocation и `skipfld` кодер ffmpeg
не выдаёт; они проверены dev-прогоном на потоках реальных кодеров
(`real_encoder_streams_parse_with_exact_bit_accounting`, см. `mem:ac3-decoder/core`).

SHA-256 (первые 16 hex): `2f1r-32k-192k` ed1815e59f99110c, `2f1r-lfe-48k-256k`
73f611ca84ba32e1, `2f2r-48k-320k` 1fd5109537422069, `3f1r-48k-256k`
f6d185a86d1b2a30, `3f1r-lfe-48k-384k` af90f325cd667e1b, `3f2r-48k-448k-cpl`
49d726f584355001, `3f-48k-256k` b2ffa13f9041c7b4, `3f-lfe-32k-256k`
677b67d2839f6c92, `5p1-32k-192k` dbd43fcc10da317a, `5p1-44k1-448k-annexd`
9ff643ac27ad3f5e, `5p1-48k-640k-annexd` 9041787c475a8d6b, `mono-48k-96k`
41a614773aecc16e, `mono-lfe-48k-128k` 2926c6579c356881, `stereo-44k1-128k-plain`
2262fb5598f7c9a6, `stereo-48k-160k-cpl-high` 64f7157fe2574ed5,
`stereo-48k-192k` a033ee1a61bd60ff, `stereo-48k-32k` 4a5dd72fef4ae9cf,
`stereo-lfe-44k1-192k` 7e7860ac5240c827.

## Синтез PCM (пакет D2)

Дополнительные потоки (тоже `generate.sh`, по 6 кадров; в `src/tests/support.rs`
это `SYNTHESIS_FIXTURES`, отдельно от таблицы BSI выше):

| файл | что проверяет |
|------|---------------|
| `overload-stereo-48k-192k.ac3` | «квадрат» 0,98 (440/1000 Гц): после декодирования пики ≈ 1,4 — выход не обрезается |
| `stereo-48k-256k-rematrix.ac3` | L ≈ R в полосах rematrixing (1500+1700 и 4500+4800 Гц, разность ненулевая), coupling выключен |
| `stereo-48k-192k-short0.ac3` | `stereo-48k-192k.ac3` с `blksw = 1` у обоих каналов в блоке 0 каждого кадра (`tools/ffmpeg_sweep.py --derive-short-blocks`); CRC не пересчитан → `CrcPolicy::Skip` |

Эталонные выдержки декодера ffmpeg n9.0.2 — `*.ffmpeg-f32le`: сырой f32le,
interleaved, порядок каналов ffmpeg (FL FR FC LFE BC SL SR), кадры
`[from, to)` — см. функцию `reference` в `generate.sh` (`atrim` по отсчётам
декодированного звука, без обрезки задержки). Как они сравниваются —
`src/tests/reference_comparison.rs` (спектральное разделение dither-а и
сетки фиксированной точки ffmpeg); полный dev-прогон — `tools/ffmpeg_sweep.py`.

SHA-256 (первые 16 hex): `overload-stereo-48k-192k` c5ae52ab1eedc6c6,
`stereo-48k-256k-rematrix` e589d9132c202e25, `stereo-48k-192k-short0`
72978f0d2c5b8c3a; выдержки: `2f1r-32k-192k` e2ab2cf52a1b9341,
`3f2r-48k-448k-cpl` bc9d73112692488c, `5p1-48k-640k-annexd` cf024dd9cb20c1e7,
`overload-stereo-48k-192k` 1ee643f2bc6e031c, `stereo-44k1-128k-plain`
49f992f9f1f68290, `stereo-48k-192k` bfd6c885e730c706, `stereo-48k-192k-short0`
dd7d43c5491f07a1, `stereo-48k-256k-rematrix` cc291ea523e5a637.
