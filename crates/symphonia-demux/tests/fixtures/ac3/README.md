# Фикстуры AC-3 / E-AC-3 в MKV и MP4/fMP4

Сгенерированы `generate.sh` (ffmpeg n9.0.2, libx264 + энкодеры `ac3`/`eac3`) из
синтетических источников: `testsrc2` 64×64 10 к/с (H.264 Constrained Baseline level 1.0,
GOP 5 → keyframe-ы 0 и 0.5 с) и 5.1(side) со своим синусом в каждом канале
(FL 300, FR 700, FC 500, LFE 60, SL 900, SR 1100 Гц, амплитуда 0.25). Длительность 1 с,
32 аудиопакета по 1536 samples на 48 кГц. Ничего не скачано, лицензия — та же, что у
репозитория. Байты зависят от версии ffmpeg, тесты опираются только на свойства ниже.

| Файл | Контейнер | Звук | Метки дорожки |
|------|-----------|------|---------------|
| `h264-ac3-5p1.mkv` | Matroska, `A_AC3` | AC-3 256k | язык deu, название «Surround» |
| `h264-eac3-5p1.mkv` | Matroska, `A_EAC3` | E-AC-3 256k | язык deu, название «Surround» |
| `h264-ac3-5p1.mp4` | MP4, `ac-3` + `dac3` | AC-3 256k | язык deu (`mdhd`) |
| `h264-eac3-5p1.mp4` | MP4, `ec-3` + `dec3` | E-AC-3 256k | язык deu (`mdhd`) |
| `audio-only-eac3-5p1.m4a` | MP4 без видео, `ec-3` + `dec3` | E-AC-3 256k | — |
| `h264-eac3-5p1-fragmented.mp4` | fMP4: `ftyp+moov`, 2×`moof+mdat`, `mfra` | E-AC-3 256k | — |

Потребители:
- `../../ac3_eac3_playback.rs` — файл → `SymphoniaDemuxer` → метаданные → пакеты →
  production декодер → тон в каждой lane; seek в середину и декод после него.
- `crates/media-source-open/src/native_startup/fmp4_dolby_vertical_tests.rs` — fMP4,
  разрезанный по top-level box-ам на init (`ftyp+moov`) и два media сегмента, отдаётся
  loopback-сервером как DASH (SegmentList) и HLS (`EXT-X-MAP`).

Эталон: `ffprobe -show_entries stream=codec_name,codec_tag_string,sample_rate,channels,channel_layout:stream_tags`
и `ffprobe -select_streams a -count_packets -show_entries stream=nb_read_packets`.
