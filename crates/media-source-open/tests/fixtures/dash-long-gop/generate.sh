#!/usr/bin/env bash
# Генерация фикстур «DASH с раздельными видео и аудио, длинный GOP» для
# crates/media-source-open/tests/dash_separate_av_long_gop.rs.
#
# Запуск: crates/media-source-open/tests/fixtures/dash-long-gop/generate.sh
# Требуется ffmpeg с libx264 и энкодером aac. Источники синтетические, ничего не
# скачивается. Свойства, на которые опирается тест (см. README.md): в видео ровно один
# keyframe (первый кадр, 25 кадров без B-frames), всё остальное - delta-кадры; аудио
# полностью лежит в одном fMP4-фрагменте. Тест сам режет файл на init и media по
# границе первого `moof`.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
common=(-hide_banner -loglevel error -y)
flags=(-fflags +bitexact -map_metadata -1 -movflags empty_moov+default_base_moof)

# Видео: 25 кадров, GOP 250 - единственный keyframe в сегменте.
ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=25:duration=1" \
    -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 -g 250 -keyint_min 250 \
    -sc_threshold 0 -pix_fmt yuv420p "${flags[@]}" "$here/video-long-gop.mp4"

# Аудио: AAC-LC стерео 48 кГц, 1 с, один фрагмент.
ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=1" \
    -ac 2 -c:a aac -b:a 64k -frag_duration 10000000 "${flags[@]}" "$here/audio.mp4"
