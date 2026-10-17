# Фикстуры AC-3 / E-AC-3

Маленькие сгенерированные elementary streams для функциональных тестов
`crates/audio/src/decoder/ac3/tests/` (читаются через `include_bytes!`,
ffmpeg во время тестов не нужен). Сгенерированы `generate.sh` (ffmpeg n9.0.2,
энкодеры `ac3` / `eac3`), лицензионных ограничений нет: сигнал — синтетические синусы.

| Файл | Кодек | Каналы | Битрейт | Длительность |
|------|-------|--------|---------|--------------|
| `tones-stereo.ac3` | AC-3 | stereo | 192k | 0.5 с, 16 syncframe-ов |
| `tones-5.1.ac3` | AC-3 | 5.1(side) | 448k | 0.5 с, 16 syncframe-ов |
| `tones-5.1.eac3` | E-AC-3 | 5.1(side) | 384k | 0.5 с, 16 syncframe-ов |

В каждом канале свой синус амплитуды 0.25 (−12 dBFS), чтобы тест по энергии
тонов доказывал раскладку каналов:

- stereo: FL 440 Гц, FR 1000 Гц;
- 5.1(side): FL 300, FR 700, FC 500, LFE 60, SL 900, SR 1100 Гц.

Эталон ffmpeg (`ffmpeg -i <файл> -f f32le -c:a pcm_f32le`): RMS каждого канала
0.17465 (константа `FFMPEG_REFERENCE_RMS` в тестах). При генерации дополнительно
сравнено посэмплово: PSNR декода fastiplayer против ffmpeg на всех каналах
всех трёх файлов 89–124 дБ (полная шкала), порядок каналов совпадает.

Те же файлы как «сырые» `.ac3`/`.eac3` используют тесты `crates/ac3-elementary-demux/tests/raw_elementary/` и сквозной тест `crates/media-source-open/src/local_media/ac3_elementary_tests.rs` (файл → demux → декодер); при перегенерации проверьте и их.
